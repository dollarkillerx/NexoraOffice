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
        name: "find_and_replace",
        title: "Find and replace",
        description: "Replace occurrences of a text, or of a regular expression, in a document. Returns how many were replaced. One undo step in the editor.",
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
                    "all": { "type": "boolean", "description": "Replace every occurrence (default) or only the first." }
                }),
                &["document", "find", "replace"],
            )
        },
    },
    ToolSpec {
        name: "get_outline",
        title: "Get outline",
        description: "The headings of a Markdown document with their levels and line numbers.",
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
        description: "The text of a document, with 1-based line numbers. Long documents come in parts: pass start_line and max_lines to read further.",
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
                    "sheet": { "type": "string", "description": "The sheet's name. Default: the sheet in front." },
                    "range": { "type": "string", "description": "A cell range like A1:D20, or one cell like B3." }
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
                    "sheet": { "type": "string", "description": "The sheet's name. Default: the sheet in front." },
                    "start": { "type": "string", "description": "The top left cell, like B2." },
                    "rows": {
                        "type": "array",
                        "description": "Rows of entries, each an array of strings.",
                        "items": { "type": "array", "items": { "type": ["string", "number", "boolean", "null"] } }
                    }
                }),
                &["document", "start", "rows"],
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
}
