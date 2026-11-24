//! What the system says about the user's region.

/// The region of the user's locale, such as `US` or `CN`, if the system
/// gives one.
pub fn region() -> Option<String> {
    region_of(&sys_locale::get_locale()?)
}

/// The region part of a locale name: `en-US`, `en_US`, `zh-Hans-CN`,
/// `es_MX.UTF-8`.
pub fn region_of(locale: &str) -> Option<String> {
    let locale = locale.split(['.', '@']).next()?;
    locale
        .split(['-', '_'])
        .skip(1)
        .find(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_ascii_uppercase)
}

/// Whether paper in the region is US Letter rather than A4.
pub fn uses_letter_paper(region: &str) -> bool {
    matches!(
        region.to_ascii_uppercase().as_str(),
        "US" | "CA"
            | "MX"
            | "PH"
            | "CL"
            | "CO"
            | "VE"
            | "CR"
            | "GT"
            | "PA"
            | "SV"
            | "DO"
            | "PR"
            | "BZ"
            | "NI"
            | "HN"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_are_read_from_locale_names() {
        assert_eq!(region_of("en-US").as_deref(), Some("US"));
        assert_eq!(region_of("en_GB").as_deref(), Some("GB"));
        assert_eq!(region_of("zh-Hans-CN").as_deref(), Some("CN"));
        assert_eq!(region_of("es_MX.UTF-8").as_deref(), Some("MX"));
        assert_eq!(region_of("ja"), None);
        assert!(uses_letter_paper("US") && uses_letter_paper("ca"));
        assert!(!uses_letter_paper("GB") && !uses_letter_paper("CN"));
    }
}
