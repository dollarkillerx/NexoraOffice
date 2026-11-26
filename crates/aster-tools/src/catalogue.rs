//! The tool catalogue: names, descriptions for the model, JSON schemas and
//! what each tool needs the policy to allow. The titles and descriptions are
//! English and written for models; the interface shows its own translated
//! names. The list is sorted by name and never changes while the program
//! runs, so a model's tool list stays byte for byte the same.

use serde_json::{Value, json};

/// What a tool does, which decides what the policy must allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Names of documents and folders, nothing of their contents.
    List,
    /// The contents of a document or file.
    Read,
    /// Changes to an open document's text. Undoable in the editor.
    Edit,
    /// A new document in a new tab.
    Create,
    /// Writing a document to disk.
    Save,
}

/// The MCP tool annotations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Annotations {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

const READ: Annotations = Annotations {
    read_only: true,
    destructive: false,
    idempotent: true,
    open_world: false,
};
const EDIT: Annotations = Annotations {
    read_only: false,
    destructive: false,
    idempotent: false,
    open_world: false,
};
const REPLACE: Annotations = Annotations {
    read_only: false,
    destructive: true,
    idempotent: false,
    open_world: false,
};

#[derive(Clone, Copy, Debug)]
pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub capability: Capability,
    pub annotations: Annotations,
    /// Whether the tool works on one document, named by its `document`
    /// argument.
    pub on_document: bool,
    schema: fn() -> Value,
}

impl ToolSpec {
    /// The JSON Schema of the arguments.
    pub fn schema(&self) -> Value {
        (self.schema)()
    }
}

fn document_only() -> Value {
    object(json!({ "document": document() }), &["document"])
}

fn document() -> Value {
    json!({
        "type": "string",
        "description": "The document's id, as list_documents gives it."
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "create_document",
        title: "Create document",
        description: "Open a new, unsaved Markdown document in a new tab with the given text. Returns its id.",
        capability: Capability::Create,
        annotations: EDIT,
        on_document: false,
        schema: || {
            object(
                json!({
                    "text": { "type": "string", "description": "The Markdown text of the new document." },
                    "title": { "type": "string", "description": "A title for the tab until the document is saved." }
                }),
                &["text"],
            )
        },
    },
    ToolSpec {
        name: "delete_blocks",
        title: "Delete blocks",
        description: "Delete block `block`, or blocks `block` to `end_block`, of a Word document: paragraphs and whole tables. The text before and after stays; a document always keeps at least one paragraph. Refused for a block that holds a picture, a footnote reference or another object (the assistant cannot see or edit those; headers, footers and footnotes are not available either). One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "block": { "type": "integer", "description": "The first block to delete, numbered from 1 as read_word gives it." },
                    "end_block": { "type": "integer", "description": "The last block to delete. Default: only `block`." }
                }),
                &["document", "block"],
            )
        },
    },
    ToolSpec {
        name: "edit_table",
        title: "Edit table",
        description: "Work on tables of a Word document. action \"insert\": make a table from `cells` (rows of texts) before or after block `block`. \"set_cell\": replace the text of the cell at `block`, `row`, `cell` (all from 1; `cell` counts the cells of the row as read_word lists them). \"insert_row\" / \"delete_row\": add `count` rows above (position \"before\") or below (\"after\", the default) row `row`, or delete row `row`. \"insert_column\" / \"delete_column\": the same for the column of cell `cell` of row `row` (default 1) left (\"before\") or right (\"after\"). Deleting the last row or column is refused: use delete_blocks to remove a table. Cells holding pictures or nested tables are refused. Headers, footers and footnotes are not available. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "action": {
                        "type": "string",
                        "enum": ["insert", "set_cell", "insert_row", "delete_row", "insert_column", "delete_column"],
                        "description": "What to do."
                    },
                    "block": { "type": "integer", "description": "The table's block number from 1 (for insert: the block the new table goes next to), as read_word gives it." },
                    "row": { "type": "integer", "description": "The row, from 1." },
                    "cell": { "type": "integer", "description": "The cell in the row, from 1." },
                    "text": { "type": "string", "description": "For set_cell: the cell's new text; line breaks make paragraphs in the cell." },
                    "cells": {
                        "type": "array",
                        "description": "For insert: the table's rows, each an array of cell texts. At most 200 rows, 20 columns and 2000 cells.",
                        "items": { "type": "array", "items": { "type": ["string", "number", "boolean", "null"] } }
                    },
                    "position": { "type": "string", "enum": ["before", "after"], "description": "Where an insert goes relative to the block, row or column. Default after." },
                    "count": { "type": "integer", "description": "For insert_row and insert_column: how many (default 1, at most 50)." }
                }),
                &["document", "action", "block"],
            )
        },
    },
    ToolSpec {
        name: "find_and_replace",
        title: "Find and replace",
        description: "Replace occurrences of a text, or of a regular expression, in a document. Returns how many were replaced. One undo step in the editor. In a Word document: the text is looked for literally (no regular expressions) in every paragraph including table cells, a replacement takes the look of the text it replaces and cannot hold line breaks, and matches touching a picture or another object are left alone.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "find": { "type": "string", "description": "The text to find." },
                    "replace": { "type": "string", "description": "What replaces it. With regex, $1 and ${name} insert groups." },
                    "regex": { "type": "boolean", "description": "Treat `find` as a regular expression. Default false." },
                    "case_sensitive": { "type": "boolean", "description": "Default true." },
                    "all": { "type": "boolean", "description": "Replace every occurrence (default) or only the first." },
                    "limit": { "type": "integer", "description": "Replace at most this many occurrences, from the start of the document." }
                }),
                &["document", "find", "replace"],
            )
        },
    },
    ToolSpec {
        name: "format_text",
        title: "Format text",
        description: "Format text in a Word document: the paragraphs of block `block` (to `end_block`), of the cell at `row` and `cell` of a table block, or only the places where `phrase` occurs in them. Character formatting (bold, italic, underline, strikethrough, color, highlight, size, font, link) can be given for a phrase or for whole paragraphs; style, align and list only for whole paragraphs. Booleans set the state (false turns it off); color and highlight take #RRGGBB or a name like red, \"none\" removes; link takes an http(s), mailto: or #bookmark address, \"\" removes it. Pictures, headers, footers and footnotes are not available. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: EDIT,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "block": { "type": "integer", "description": "The first block, numbered from 1 as read_word gives it. For a table: with `row` and `cell`." },
                    "end_block": { "type": "integer", "description": "The last block. Default: only `block`." },
                    "row": { "type": "integer", "description": "With a table block: the row, from 1." },
                    "cell": { "type": "integer", "description": "With `row`: the cell in the row, from 1 (default 1)." },
                    "phrase": { "type": "string", "description": "Format only the places where this text occurs (every one, literally). Without it, whole paragraphs." },
                    "case_sensitive": { "type": "boolean", "description": "For `phrase`. Default true." },
                    "bold": { "type": "boolean" },
                    "italic": { "type": "boolean" },
                    "underline": { "type": "boolean" },
                    "strikethrough": { "type": "boolean" },
                    "color": { "type": "string", "description": "Text colour: #RRGGBB or a name, \"none\" for automatic." },
                    "highlight": { "type": "string", "description": "Highlight colour: #RRGGBB or a name, \"none\" to remove." },
                    "size": { "type": "number", "description": "Size in points." },
                    "font": { "type": "string", "description": "A typeface name; \"default\" for the document's own." },
                    "link": { "type": "string", "description": "A web, mailto: or #bookmark address; \"\" removes the link." },
                    "style": { "type": "string", "description": "A paragraph style's name as read_word shows it, for example \"heading 1\". Whole paragraphs only." },
                    "align": { "type": "string", "enum": ["left", "center", "right", "justify"], "description": "Whole paragraphs only." },
                    "list": { "type": "string", "enum": ["bullet", "numbered", "none"], "description": "Make the paragraphs a list, or take them out of it. Whole paragraphs only." }
                }),
                &["document", "block"],
            )
        },
    },
    ToolSpec {
        name: "get_outline",
        title: "Get outline",
        description: "The headings of a document with their levels and line numbers (in a Word document: the block number of each heading and its page).",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: document_only,
    },
    ToolSpec {
        name: "get_selection",
        title: "Get selection",
        description: "The text the user has selected in a document, and the line and column range of the selection. Empty text means only a caret. In a spreadsheet: the sheet, the selected range and the active cell.",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: document_only,
    },
    ToolSpec {
        name: "insert_paragraphs",
        title: "Insert paragraphs",
        description: "Insert new paragraphs into a Word document before or after block `block`. Each line of `text` is a paragraph. They are formatted like the paragraph they go next to (after a heading, as the body text that follows; list items continue the list) unless `style` names one. Pictures, headers, footers and footnotes are not available. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: EDIT,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "block": { "type": "integer", "description": "The block to insert next to, numbered from 1 as read_word gives it. A table is a block too." },
                    "position": { "type": "string", "enum": ["before", "after"], "description": "Default after." },
                    "text": { "type": "string", "description": "The new text; line breaks separate paragraphs. At most 100000 characters and 2000 paragraphs." },
                    "style": { "type": "string", "description": "A paragraph style's name as read_word lists them, for example \"heading 2\"." }
                }),
                &["document", "block", "text"],
            )
        },
    },
    ToolSpec {
        name: "insert_text",
        title: "Insert text",
        description: "Insert text into a document at the caret, at its start, at its end, or before a line.",
        capability: Capability::Edit,
        annotations: EDIT,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "text": { "type": "string", "description": "The text to insert." },
                    "position": {
                        "type": "string",
                        "enum": ["cursor", "start", "end", "line"],
                        "description": "Where to insert. Default cursor."
                    },
                    "line": { "type": "integer", "description": "With position \"line\": the 1-based line the text goes before." }
                }),
                &["document", "text"],
            )
        },
    },
    ToolSpec {
        name: "list_documents",
        title: "List documents",
        description: "The documents open in Aster Office that you may see: id, title, file path, kind, whether they have unsaved changes, and your access level to each.",
        capability: Capability::List,
        annotations: READ,
        on_document: false,
        schema: || object(json!({}), &[]),
    },
    ToolSpec {
        name: "list_folder",
        title: "List folder",
        description: "The files and folders in a folder you have access to.",
        capability: Capability::List,
        annotations: READ,
        on_document: false,
        schema: || {
            object(
                json!({ "path": { "type": "string", "description": "An absolute folder path." } }),
                &["path"],
            )
        },
    },
    ToolSpec {
        name: "open_file",
        title: "Open file",
        description: "Open a Markdown or text file from a folder you have access to in a new tab (or switch to it if it is open). Returns its document id.",
        capability: Capability::Read,
        annotations: READ,
        on_document: false,
        schema: || {
            object(
                json!({ "path": { "type": "string", "description": "An absolute file path." } }),
                &["path"],
            )
        },
    },
    ToolSpec {
        name: "read_document",
        title: "Read document",
        description: "The text of a document, with 1-based line numbers. Long documents come in parts: pass start_line and max_lines to read further. For a Word document use read_word, which gives the block numbers the editing tools take.",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "start_line": { "type": "integer", "description": "The first line to read, 1-based. Default 1." },
                    "max_lines": { "type": "integer", "description": "At most this many lines. Default 400." }
                }),
                &["document"],
            )
        },
    },
    ToolSpec {
        name: "read_sheet",
        title: "Read sheet",
        description: "Cells of a spreadsheet document as JSON: the values as the grid shows them, row by row from first_row, with formulas listed apart by cell. Without a range, the used part of the sheet (at most 200 rows and 26 columns). Also gives the names of all sheets.",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "sheet": { "type": "string", "description": "The sheet's name. Without it, the sheet in front when the call runs; the result names the sheet read." },
                    "range": { "type": "string", "description": "A cell range like A1:D20, or one cell like B3." }
                }),
                &["document"],
            )
        },
    },
    ToolSpec {
        name: "read_word",
        title: "Read Word document",
        description: "The body of a Word document as JSON, block by block. Every block has its number from 1 (`block`): a paragraph with its style name, text, list kind, alignment and links, or a table with its rows of cell texts (rows and cells are numbered from 1 in the order listed). The editing tools take these numbers. Long documents come in parts: pass start_block and max_blocks (default 60) to read on; a long table is read with start_row and max_rows. Also gives the paragraph styles the document has, and, for reading only, the text of headers, footers, footnotes, endnotes and comments and whether there are tracked changes. The editing tools work on the body only. Pictures are not shown.",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "start_block": { "type": "integer", "description": "The first block to read, from 1. Default 1." },
                    "max_blocks": { "type": "integer", "description": "At most this many blocks. Default 60, at most 300." },
                    "start_row": { "type": "integer", "description": "If the first block read is a table: its first row, from 1." },
                    "max_rows": { "type": "integer", "description": "At most this many rows of each table. Default 50, at most 200." }
                }),
                &["document"],
            )
        },
    },
    ToolSpec {
        name: "replace_lines",
        title: "Replace lines",
        description: "Replace lines start_line to end_line (1-based, inclusive) of a document with new text. An empty text deletes them. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "start_line": { "type": "integer" },
                    "end_line": { "type": "integer" },
                    "text": { "type": "string", "description": "The new text for those lines, without a final line break." }
                }),
                &["document", "start_line", "end_line", "text"],
            )
        },
    },
    ToolSpec {
        name: "replace_paragraphs",
        title: "Replace paragraphs",
        description: "Replace the text of block `block`, or of blocks `block` to `end_block`, of a Word document with new text (line breaks make paragraphs; the first paragraph keeps its formatting and the look of its first words). Tables are not paragraphs: use edit_table for their cells. Refused for a paragraph that holds a picture, a footnote reference or another object (use find_and_replace to change its words). Headers, footers and footnotes are not available. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "block": { "type": "integer", "description": "The first block, numbered from 1 as read_word gives it." },
                    "end_block": { "type": "integer", "description": "The last block. Default: only `block`." },
                    "text": { "type": "string", "description": "The new text. At most 100000 characters and 2000 paragraphs." }
                }),
                &["document", "block", "text"],
            )
        },
    },
    ToolSpec {
        name: "replace_selection",
        title: "Replace selection",
        description: "Replace the user's selection in a document with new text (or insert it at the caret). One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "text": { "type": "string" }
                }),
                &["document", "text"],
            )
        },
    },
    ToolSpec {
        name: "save_document",
        title: "Save document",
        description: "Save a document to its file. A document that was never saved cannot be saved this way.",
        capability: Capability::Save,
        annotations: Annotations {
            read_only: false,
            destructive: true,
            idempotent: true,
            open_world: false,
        },
        on_document: true,
        schema: document_only,
    },
    ToolSpec {
        name: "search_document",
        title: "Search document",
        description: "Find a text or regular expression in a document. Returns the matching lines with their line numbers, at most 200.",
        capability: Capability::Read,
        annotations: READ,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "query": { "type": "string" },
                    "regex": { "type": "boolean", "description": "Default false." },
                    "case_sensitive": { "type": "boolean", "description": "Default false." }
                }),
                &["document", "query"],
            )
        },
    },
    ToolSpec {
        name: "write_cells",
        title: "Write cells",
        description: "Type entries into the cells of a spreadsheet document, starting at a cell and going right and down, as a person typing them would: numbers, dates, percentages, TRUE/FALSE, formulas starting with = and text. An empty entry empties its cell. One undo step in the editor.",
        capability: Capability::Edit,
        annotations: REPLACE,
        on_document: true,
        schema: || {
            object(
                json!({
                    "document": document(),
                    "sheet": { "type": "string", "description": "The sheet's name, as get_selection, read_sheet or the context gives it." },
                    "start": { "type": "string", "description": "The top left cell, like B2." },
                    "rows": {
                        "type": "array",
                        "description": "Rows of entries, each an array of strings.",
                        "items": { "type": "array", "items": { "type": ["string", "number", "boolean", "null"] } }
                    }
                }),
                &["document", "sheet", "start", "rows"],
            )
        },
    },
];

/// Every tool, sorted by name.
pub fn tools() -> &'static [ToolSpec] {
    TOOLS
}

pub fn find(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_sorted_and_every_schema_is_an_object() {
        let names: Vec<&str> = tools().iter().map(|tool| tool.name).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        for tool in tools() {
            let schema = tool.schema();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            assert_eq!(schema["additionalProperties"], false, "{}", tool.name);
            let has_document = schema["properties"].get("document").is_some();
            assert_eq!(has_document, tool.on_document, "{}", tool.name);
            assert_eq!(
                tool.annotations.read_only,
                matches!(tool.capability, Capability::List | Capability::Read),
                "{}",
                tool.name
            );
        }
        assert!(find("read_document").is_some());
        assert!(find("run_command").is_none());
    }

    /// The tools for Word documents: one reader, six writers that all go
    /// through the policy as edits, and descriptions that say what they do
    /// not do.
    #[test]
    fn the_word_tools_are_declared_the_way_the_spreadsheet_tools_are() {
        let writers = [
            "replace_paragraphs",
            "insert_paragraphs",
            "delete_blocks",
            "format_text",
            "edit_table",
        ];
        let reader = find("read_word").expect("read_word");
        assert_eq!(reader.capability, Capability::Read);
        assert!(reader.annotations.read_only && reader.on_document);
        for name in writers.into_iter().chain(["find_and_replace"]) {
            let tool = find(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(tool.capability, Capability::Edit, "{name}");
            assert!(tool.on_document && !tool.annotations.read_only, "{name}");
            assert!(tool.description.contains("undo step"), "{name}");
            let schema = tool.schema();
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    schema["properties"]
                        .get(required.as_str().unwrap())
                        .is_some(),
                    "{name} requires an argument it does not declare"
                );
            }
        }
        for name in writers {
            let tool = find(name).unwrap();
            // What the model cannot do is said where it reads about the tool.
            let description = tool.description.to_lowercase();
            assert!(
                description.contains("footnotes") && description.contains("headers"),
                "{name}"
            );
            // Every argument says what it is, except the plain switches.
            for (argument, property) in tool.schema()["properties"].as_object().unwrap() {
                if property["type"] != "boolean" && argument != "document" {
                    assert!(
                        property["description"].is_string() || property["enum"].is_array(),
                        "{name}.{argument}"
                    );
                }
            }
        }
        // The block numbers are what ties the reader to the writers.
        for name in ["replace_paragraphs", "insert_paragraphs", "delete_blocks"] {
            assert_eq!(
                find(name).unwrap().schema()["properties"]["block"]["type"],
                "integer"
            );
            assert!(
                find(name).unwrap().schema()["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("block"))
            );
        }
        assert!(
            find("read_document")
                .unwrap()
                .description
                .contains("read_word")
        );
    }
}
