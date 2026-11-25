//! Limits of PdfView: memory held, odd page sizes, and requests that settle.

use aster_surface::pdf_view::{PIXEL_BUDGET, PdfView};
use std::time::{Duration, Instant};

/// What a worker would deliver for a request: the page at `scale`, capped.
fn render(pages: &[(f32, f32)], page: usize, scale: f32, cap: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = pages[page];
    let w = ((w * scale) as u32).clamp(1, cap);
    let h = ((h * scale) as u32).clamp(1, cap);
    (w, h, vec![7; (w * h * 4) as usize])
}

#[test]
fn pixels_held_never_exceed_the_budget() {
    let pages = vec![(1_000_000.0f32, 1_000_000.0f32); 60];
    let mut view = PdfView::new(pages.clone());
    view.resize(1400, 900, 1.0);
    assert_eq!(PIXEL_BUDGET, 256 << 20);
    let budget = 8 << 20;
    view.set_pixel_budget(budget);
    for step in 0..40 {
        for (page, scale) in view.wanted() {
            // 1000 x 1000 = 4 MB each.
            let (w, h, px) = render(&pages, page, scale, 1000);
            view.insert(page, scale, w, h, px);
            // What is on screen stays even if it alone is over the budget.
            assert!(
                view.pixel_bytes() <= budget.max(view.visible_bytes()),
                "{}",
                view.pixel_bytes()
            );
        }
        let _ = view.render();
        view.wheel(10.0, 0.0, 300.0);
        view.go_to(step);
    }
}

#[test]
fn an_image_larger_than_the_budget_is_not_kept() {
    let mut view = PdfView::new(vec![(100.0, 100.0)]);
    view.resize(1000, 800, 1.0);
    view.set_pixel_budget(1000);
    assert!(!view.insert(0, 1.0, 100, 100, vec![0; 40_000]));
    assert_eq!(view.pixel_bytes(), 0);
    // It is not asked for again.
    assert!(view.wanted().iter().all(|(page, _)| *page != 0));
}

#[test]
fn drop_pixels_frees_everything_and_the_view_recovers() {
    let pages = vec![(612.0f32, 792.0f32); 5];
    let mut view = PdfView::new(pages.clone());
    view.resize(1000, 800, 1.0);
    let first = view.wanted();
    for (page, scale) in &first {
        let (w, h, px) = render(&pages, *page, *scale, 2000);
        view.insert(*page, *scale, w, h, px);
    }
    assert!(view.pixel_bytes() > 0);
    assert!(view.wanted().is_empty());
    view.drop_pixels();
    assert_eq!(view.pixel_bytes(), 0);
    // Everything is wanted again, and drawing still works afterwards.
    assert_eq!(view.wanted(), first);
    let frame = view.render();
    assert_eq!((frame.width(), frame.height()), (1000, 800));
}

#[test]
fn equal_thumbnail_and_main_scale_settles() {
    // The wide page forces fit-width to the 10 % clamp (0.1 px/pt); the
    // second page's thumbnail scale is 0.1 px/pt too.
    let pages = vec![(14_400.0f32, 1.0f32), (1320.0, 1320.0)];
    let mut view = PdfView::new(pages.clone());
    view.resize(1000, 800, 1.0);
    for _ in 0..4 {
        for (page, scale) in view.wanted() {
            let (w, h, px) = render(&pages, page, scale, 4000);
            view.insert(page, scale, w, h, px);
        }
    }
    assert!(view.wanted().is_empty(), "{:?}", view.wanted());
}

#[test]
fn equal_scales_settle_when_the_main_page_is_far_away() {
    // Thumbnails of far pages are asked for at the main scale; they must
    // stay even though main renderings of far pages are forgotten.
    let pages = vec![(1320.0f32, 1320.0f32); 40];
    let mut view = PdfView::new(pages.clone());
    view.resize(1000, 800, 1.0);
    view.set_zoom(aster_surface::pdf_view::Zoom::Percent(10.0));
    for _ in 0..4 {
        for (page, scale) in view.wanted() {
            let (w, h, px) = render(&pages, page, scale, 4000);
            view.insert(page, scale, w, h, px);
        }
    }
    assert!(view.wanted().is_empty(), "{:?}", view.wanted());
}

#[test]
fn absurd_page_sizes_stay_finite_and_fast() {
    let started = Instant::now();
    for size in [3.0e38f32, 1.0e20, 1.0e9, f32::INFINITY, f32::NAN, -5.0, 0.0] {
        let mut view = PdfView::new(vec![(size, size), (1.0, size), (size, 1.0)]);
        view.resize(1200, 800, 2.0);
        let _ = view.render();
        view.wheel(1000.0, 1.0e30, 1.0e30);
        let _ = view.render();
        view.set_scroll(f64::MAX);
        let _ = view.render();
        let (offset, max) = view.scroll_range();
        assert!(offset.is_finite() && max.is_finite(), "{size}");
        assert!(view.pixels_per_point().is_finite() && view.pixels_per_point() > 0.0);
        let wanted = view.wanted();
        assert!(wanted.len() < 100, "{size}: {}", wanted.len());
        assert!(
            wanted.iter().all(|(_, s)| s.is_finite() && *s > 0.0),
            "{size}"
        );
        assert!(view.current() < 3);
    }
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn a_full_screen_of_big_pages_settles_within_budget_plus_what_is_visible() {
    // Several pages on screen at once, each as big as a worker delivers
    // (4096 x 4096 = 64 MiB): more together than the 256 MB budget.
    let pages = vec![(100.0f32, 100.0f32); 8];
    let mut view = PdfView::new(pages);
    view.set_strip(false);
    view.resize(1000, 4900, 1.0);
    let one = 4096 * 4096 * 4;
    let fill = |view: &mut PdfView| {
        let wanted = view.wanted();
        for (page, scale) in &wanted {
            view.insert(*page, *scale, 4096, 4096, vec![0; one]);
        }
        wanted.len()
    };
    for _ in 0..6 {
        fill(&mut view);
        let _ = view.render();
    }
    assert!(view.wanted().is_empty(), "{:?}", view.wanted());
    let held = view.pixel_bytes();
    assert!(held >= 5 * one, "{held}");
    assert!(held <= PIXEL_BUDGET + 5 * one, "{held}");
    // It stays settled over more frames.
    for _ in 0..3 {
        let _ = view.render();
        assert_eq!(fill(&mut view), 0);
    }
}

#[test]
fn a_clamped_page_is_asked_for_at_a_scale_that_fits_the_screen() {
    // Laid out as 14 400 pt, really 1e6 pt: the worker must be asked for a
    // scale that makes the real page as big as the rectangle shown.
    let real = 1_000_000.0f32;
    let mut view = PdfView::new(vec![(real, real), (612.0, 792.0)]);
    view.resize(1400, 900, 1.0);
    let shown = f64::from(14_400.0f32) * view.pixels_per_point();
    for _ in 0..3 {
        let wanted = view.wanted();
        for (page, scale) in wanted {
            if page == 0 {
                let side = f64::from(real) * f64::from(scale);
                // Main page or thumbnail (132 wide): never more than shown.
                assert!(side <= shown.max(133.0) + 1.0, "{side} vs {shown}");
                let n = side.round() as u32;
                view.insert(0, scale, n, n, vec![0; (n * n * 4) as usize]);
            } else {
                view.insert(page, scale, 10, 10, vec![0; 400]);
            }
        }
    }
    assert!(view.wanted().is_empty(), "{:?}", view.wanted());
    assert!(view.pixel_bytes() < 16 << 20);
}
