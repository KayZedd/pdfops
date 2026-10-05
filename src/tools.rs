//! The command registry behind the MCP server and `pdfops tools`.

use anyhow::Result;
use serde_json::{Value, json};

use crate::ops::{
    annotate, assemble, create, edit, forms, layout, ocr, read, redact, render, scan, sign,
};

pub struct Tool {
    /// CLI subcommand name, e.g. `set-meta`.
    pub name: &'static str,
    pub schema: fn() -> Value,
    pub call: fn(Value) -> Result<Value>,
}

macro_rules! tool {
    ($name:literal, $args:ty, $run:path) => {
        Tool {
            name: $name,
            schema: || {
                let mut schema =
                    serde_json::to_value(schemars::schema_for!($args)).expect("schema serialises");
                // Draft and title are noise for a model reading the schema.
                if let Some(map) = schema.as_object_mut() {
                    map.remove("$schema");
                    map.remove("title");
                }
                schema
            },
            call: |v| $run(serde_json::from_value::<$args>(v)?),
        }
    };
}

pub const TOOLS: &[Tool] = &[
    tool!("info", read::InfoArgs, read::info),
    tool!("text", read::TextArgs, read::text),
    tool!("search", read::SearchArgs, read::search),
    tool!("layout", layout::LayoutArgs, layout::layout),
    tool!("tables", layout::TablesArgs, layout::tables),
    tool!("ocr", ocr::OcrArgs, ocr::ocr),
    tool!("ocr-langs", ocr::OcrLangsArgs, ocr::ocr_langs),
    tool!("ocr-install", ocr::OcrInstallArgs, ocr::ocr_install),
    tool!("outline", read::OutlineArgs, read::outline),
    tool!(
        "annotations",
        annotate::AnnotationsArgs,
        annotate::annotations
    ),
    tool!("scan", scan::ScanArgs, scan::scan),
    tool!("render", render::RenderArgs, render::render),
    tool!("images", render::ImagesArgs, render::images),
    tool!("create", create::CreateArgs, create::create),
    tool!("merge", assemble::MergeArgs, assemble::merge),
    tool!("pages", assemble::PagesArgs, assemble::pages),
    tool!("split", assemble::SplitArgs, assemble::split),
    tool!("rotate", edit::RotateArgs, edit::rotate),
    tool!("stamp", edit::StampArgs, edit::stamp),
    tool!("annotate", annotate::AnnotateArgs, annotate::annotate),
    tool!("redact", redact::RedactArgs, redact::redact),
    tool!("replace", redact::ReplaceArgs, redact::replace),
    tool!("sanitize", scan::SanitizeArgs, scan::sanitize),
    tool!("set-meta", edit::SetMetaArgs, edit::set_meta),
    tool!("compress", edit::CompressArgs, edit::compress),
    tool!("encrypt", edit::EncryptArgs, edit::encrypt),
    tool!("decrypt", edit::DecryptArgs, edit::decrypt),
    tool!("sign", sign::SignArgs, sign::sign),
    tool!("signatures", sign::SignaturesArgs, sign::signatures),
    tool!("forms", forms::FormsArgs, forms::forms),
    tool!("fill", forms::FillArgs, forms::fill),
];

/// Name used for the tool over MCP and in function-calling definitions.
pub fn tool_name(tool: &Tool) -> String {
    format!("pdf_{}", tool.name.replace('-', "_"))
}

/// Tool definitions in the shape MCP and most function-calling APIs expect.
pub fn definitions() -> Vec<Value> {
    let cli = <crate::cli::Cli as clap::CommandFactory>::command();
    TOOLS
        .iter()
        .map(|t| {
            let about = cli.find_subcommand(t.name).and_then(|c| c.get_about()).map(|a| a.to_string());
            json!({"name": tool_name(t), "description": about.unwrap_or_default(), "inputSchema": (t.schema)()})
        })
        .collect()
}

/// Runs a tool by its MCP name with JSON arguments.
pub fn call(name: &str, args: Value) -> Result<Value> {
    let tool = TOOLS
        .iter()
        .find(|t| tool_name(t) == name)
        .ok_or_else(|| anyhow::anyhow!("unknown tool '{name}'"))?;
    crate::sandbox::check_args(&args)?;
    // The inputs are noted first: the call consumes its arguments.
    let inputs: Vec<std::path::PathBuf> = args
        .get("input")
        .into_iter()
        .chain(
            args.get("inputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten(),
        )
        .filter_map(|p| p.as_str().map(Into::into))
        .collect();
    let mut result = (tool.call)(args)?;
    note_repairs(&mut result, |path| inputs.iter().any(|i| i == path));
    Ok(result)
}

/// Adds `repaired_inputs` to a result when a damaged input had to be rebuilt to produce it.
///
/// A caller should know: what could not be read from such a file is missing from the output.
pub fn note_repairs(result: &mut Value, mine: impl Fn(&std::path::Path) -> bool) {
    let repaired: Vec<std::path::PathBuf> = crate::doc::repaired_inputs()
        .into_iter()
        .filter(|path| mine(path))
        .collect();
    if !repaired.is_empty()
        && let Some(map) = result.as_object_mut()
    {
        map.insert("repaired_inputs".into(), json!(repaired));
    }
}
