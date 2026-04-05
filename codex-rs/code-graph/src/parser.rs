use crate::builder::FileGraphEntry;
use crate::types::GraphNode;
use crate::types::GraphNodeImportance;
use crate::types::GraphNodeType;
use crate::types::LineSpan;
use crate::types::NodeRecord;
use anyhow::Context;
use ignore::WalkBuilder;
use sha1::Digest;
use sha1::Sha1;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::MAIN_SEPARATOR;
use std::path::Path;
use syn::Item;
use syn::visit::Visit;

pub(crate) fn parse_workspace(
    roots: &[String],
    previous_files: &BTreeMap<String, FileGraphEntry>,
) -> anyhow::Result<BTreeMap<String, FileGraphEntry>> {
    let mut files = previous_files.clone();

    for root in roots {
        let mut walk = WalkBuilder::new(root);
        walk.standard_filters(true);
        walk.hidden(false);
        for entry in walk.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            if !entry
                .file_type()
                .is_some_and(|file_type| file_type.is_file())
            {
                continue;
            }

            let relative_path = relative_path_string(Path::new(root), path)?;
            let bytes = fs::read(path)
                .with_context(|| format!("failed to read source file {}", path.display()))?;
            let file_hash = sha1_hex(&bytes);
            let content = String::from_utf8_lossy(&bytes).into_owned();

            if let Some(existing) = previous_files.get(&relative_path)
                && existing.file_hash == file_hash
            {
                files.insert(relative_path.clone(), existing.clone());
                continue;
            }

            let nodes = parse_file_nodes(
                &relative_path,
                &content,
                path.extension().and_then(|ext| ext.to_str()) == Some("rs"),
            )?;
            files.insert(
                relative_path.clone(),
                FileGraphEntry {
                    root: root.clone(),
                    relative_path,
                    file_hash,
                    nodes,
                },
            );
        }
    }

    Ok(files)
}

fn parse_file_nodes(
    relative_path: &str,
    content: &str,
    is_rust: bool,
) -> anyhow::Result<Vec<NodeRecord>> {
    let mut records = Vec::new();
    let module_file = if is_rust {
        relative_path.to_string()
    } else {
        relative_path.replace('/', &MAIN_SEPARATOR.to_string())
    };
    records.push(NodeRecord {
        node: GraphNode {
            id: format!("module::{relative_path}"),
            node_type: GraphNodeType::Module,
            file: module_file,
            what_it_is: format!("File module `{relative_path}`"),
            behavior_summary: format!(
                "Represents the file at {relative_path} and provides the graph entry used for file search."
            ),
            calls: Vec::new(),
            triggered_by: Vec::new(),
            affects: Vec::new(),
            side_effects: Vec::new(),
            importance: GraphNodeImportance::Low,
        },
        local_calls: Vec::new(),
        imports: Vec::new(),
        span: None,
    });

    if !is_rust {
        records.extend(parse_non_rust_nodes(relative_path, content)?);
        return Ok(records);
    }

    records.extend(parse_rust_nodes(relative_path, content)?);
    Ok(records)
}

fn parse_rust_nodes(relative_path: &str, content: &str) -> anyhow::Result<Vec<NodeRecord>> {
    let syntax = syn::parse_file(content)
        .with_context(|| format!("failed to parse Rust source for {relative_path}"))?;

    let module_path = module_path_from_relative(relative_path, true);
    let import_map = collect_rust_imports(&syntax, &module_path);
    let function_names = syntax
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Fn(function) => Some(function.sig.ident.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();

    let mut records = Vec::new();
    for item in &syntax.items {
        if let Item::Fn(function) = item {
            let name = function.sig.ident.to_string();
            let node_id = format!("{module_path}::{name}");
            let calls = collect_calls(&function.block);
            let resolved_calls = calls
                .into_iter()
                .map(|call| {
                    resolve_rust_call_name(&call, &module_path, &import_map, &function_names)
                })
                .collect::<Vec<_>>();
            let side_effects = infer_side_effects(&name, &content);
            let span = span_for_function(content, &name);
            let behavior_summary = summarize_behavior(
                &name,
                &name,
                extract_doc_comments(
                    content,
                    span.as_ref().map_or(0, |span| span.start_line),
                    Language::Rust,
                ),
                &resolved_calls,
            );
            let what_it_is = format!("Rust function `{name}`");

            records.push(NodeRecord {
                node: GraphNode {
                    id: node_id,
                    node_type: GraphNodeType::Function,
                    file: relative_path.to_string(),
                    what_it_is,
                    behavior_summary,
                    calls: resolved_calls.clone(),
                    triggered_by: Vec::new(),
                    affects: resolved_calls.clone(),
                    side_effects,
                    importance: importance_for_name(&name),
                },
                local_calls: resolved_calls,
                imports: import_map.values().cloned().collect(),
                span,
            });
        }
    }
    Ok(records)
}

fn parse_non_rust_nodes(relative_path: &str, content: &str) -> anyhow::Result<Vec<NodeRecord>> {
    let language = detect_non_rust_language(relative_path);
    let module_path = module_path_from_relative(relative_path, false);
    let import_map = collect_non_rust_imports(content, language, relative_path);
    let mut records = Vec::new();

    for definition in collect_non_rust_function_definitions(content, language) {
        let name = definition.name;
        let node_id = format!("{module_path}::{name}");
        let calls = collect_non_rust_calls(&definition.body);
        let resolved_calls = calls
            .into_iter()
            .map(|call| resolve_non_rust_call_name(&call, &module_path, &import_map))
            .collect::<Vec<_>>();
        let side_effects = infer_side_effects(&name, &definition.body);
        let language_name = match language {
            Some(NonRustLanguage::Python) => "Python",
            Some(NonRustLanguage::JsLike) => "JavaScript/TypeScript",
            None => "Function",
        };
        let behavior_summary = summarize_behavior(
            &name,
            &definition.body,
            extract_doc_comments(
                content,
                definition.span.as_ref().map_or(0, |span| span.start_line),
                language.map(Into::into).unwrap_or(Language::JsLike),
            ),
            &resolved_calls,
        );
        let what_it_is = format!("{language_name} function `{name}`");
        let span = definition.span;

        records.push(NodeRecord {
            node: GraphNode {
                id: node_id,
                node_type: GraphNodeType::Function,
                file: relative_path.to_string(),
                what_it_is,
                behavior_summary,
                calls: resolved_calls.clone(),
                triggered_by: Vec::new(),
                affects: resolved_calls.clone(),
                side_effects,
                importance: importance_for_name(&name),
            },
            local_calls: resolved_calls,
            imports: import_map.values().cloned().collect(),
            span,
        });
    }

    Ok(records)
}

fn module_path_from_relative(relative_path: &str, is_rust: bool) -> String {
    let mut path = relative_path.replace('/', "::");
    if is_rust {
        if path.ends_with("::mod.rs") {
            path = path.trim_end_matches("::mod.rs").to_string();
        } else if path.ends_with(".rs") {
            path = path.trim_end_matches(".rs").to_string();
        }
        if path == "lib" || path == "main" || path.is_empty() {
            "crate".to_string()
        } else {
            format!("crate::{path}")
        }
    } else {
        if let Some(index) = path.rfind('.') {
            path.truncate(index);
        }
        if path == "" {
            "crate".to_string()
        } else {
            format!("crate::{path}")
        }
    }
}

fn collect_rust_imports(
    syntax: &syn::File,
    module_path: &str,
) -> std::collections::HashMap<String, String> {
    let mut imports = std::collections::HashMap::new();
    for item in &syntax.items {
        if let Item::Use(item_use) = item {
            collect_use_tree(&item_use.tree, module_path, &mut Vec::new(), &mut imports);
        }
    }
    imports
}

fn collect_use_tree(
    tree: &syn::UseTree,
    module_path: &str,
    prefix: &mut Vec<String>,
    imports: &mut std::collections::HashMap<String, String>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            collect_use_tree(&path.tree, module_path, prefix, imports);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let mut full_path = normalize_import_prefix(prefix, module_path);
            full_path.push_str("::");
            full_path.push_str(&name.ident.to_string());
            imports.insert(name.ident.to_string(), full_path);
        }
        syn::UseTree::Rename(rename) => {
            let mut full_path = normalize_import_prefix(prefix, module_path);
            full_path.push_str("::");
            full_path.push_str(&rename.ident.to_string());
            imports.insert(rename.rename.to_string(), full_path);
        }
        syn::UseTree::Glob(_) => {
            // Glob imports are not fully resolved here.
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                collect_use_tree(tree, module_path, prefix, imports);
            }
        }
    }
}

fn normalize_import_prefix(prefix: &[String], module_path: &str) -> String {
    if prefix.is_empty() {
        return "crate".to_string();
    }

    let mut result_segments = Vec::new();
    match prefix[0].as_str() {
        "crate" => result_segments.extend(prefix.iter().cloned()),
        "self" => {
            result_segments = module_path.split("::").map(str::to_string).collect();
            result_segments.pop();
            result_segments.extend(prefix.iter().skip(1).cloned());
        }
        "super" => {
            result_segments = module_path.split("::").map(str::to_string).collect();
            result_segments.pop();
            result_segments.pop();
            result_segments.extend(prefix.iter().skip(1).cloned());
        }
        _ => {
            result_segments.push("crate".to_string());
            result_segments.extend(prefix.iter().cloned());
        }
    }

    if result_segments.is_empty() {
        "crate".to_string()
    } else {
        result_segments.join("::")
    }
}

fn resolve_rust_call_name(
    call: &str,
    module_path: &str,
    import_map: &std::collections::HashMap<String, String>,
    local_defs: &[String],
) -> String {
    if call.contains("::") {
        if call.starts_with("self::") {
            return format!("{module_path}::{}", call.trim_start_matches("self::"));
        }
        if call.starts_with("super::") {
            let mut module_parts = module_path.split("::").map(str::to_string).collect::<Vec<_>>();
            module_parts.pop();
            let base = if module_parts.is_empty() {
                "crate".to_string()
            } else {
                module_parts.join("::")
            };
            return format!("{base}::{}", call.trim_start_matches("super::"));
        }
        return call.to_string();
    }
    if let Some(mapped) = import_map.get(call) {
        return mapped.clone();
    }
    if local_defs.contains(&call.to_string()) {
        return format!("{module_path}::{call}");
    }
    format!("crate::{call}")
}

fn collect_non_rust_imports(
    content: &str,
    language: Option<NonRustLanguage>,
    relative_path: &str,
) -> std::collections::HashMap<String, String> {
    let mut imports = std::collections::HashMap::new();
    let module_path = module_path_from_relative(relative_path, false);
    for line in content.lines() {
        let trimmed = line.trim_start();
        match language {
            Some(NonRustLanguage::Python) => {
                if let Some(rest) = trimmed.strip_prefix("from ") {
                    if let Some((module, import_list)) = rest.split_once(" import ") {
                        for part in import_list.split(',') {
                            let part = part.trim();
                            let (name, alias) = if let Some((original, alias)) = part.split_once(" as ") {
                                (original.trim(), alias.trim())
                            } else {
                                (part, part)
                            };
                            let full_path = format!("{module_path}::{}::{}", module.replace('.', "::"), name);
                            imports.insert(alias.to_string(), full_path);
                        }
                    }
                } else if let Some(rest) = trimmed.strip_prefix("import ") {
                    for part in rest.split(',') {
                        let part = part.trim();
                        let (name, alias) = if let Some((original, alias)) = part.split_once(" as ") {
                            (original.trim(), alias.trim())
                        } else {
                            (part, part)
                        };
                        let full_path = format!("{module_path}::{}", name.replace('.', "::"));
                        imports.insert(alias.to_string(), full_path);
                    }
                }
            }
            Some(NonRustLanguage::JsLike) => {
                if trimmed.starts_with("import ") {
                    if let Some(rest) = trimmed.strip_prefix("import ") {
                        if rest.contains(" from ") {
                            let parts = rest.splitn(2, " from ").collect::<Vec<_>>();
                            let import_clause = parts[0].trim();
                            let source = parts[1].trim().trim_matches('"').trim_matches('"').trim_matches('`').trim_matches('"').trim_matches('`');
                            let source_module = source.replace('/', "::").trim_start_matches("./").trim_start_matches("../").to_string();
                            if import_clause.starts_with('{') {
                                let names = import_clause.trim_matches('{').trim_matches('}');
                                for part in names.split(',') {
                                    let part = part.trim();
                                    let (name, alias) = if let Some((original, alias)) = part.split_once(" as ") {
                                        (original.trim(), alias.trim())
                                    } else {
                                        (part, part)
                                    };
                                    let full_path = format!("crate::{source_module}::{name}");
                                    imports.insert(alias.to_string(), full_path);
                                }
                            } else {
                                let name = import_clause.split_whitespace().next().unwrap_or(import_clause);
                                let full_path = format!("crate::{source_module}");
                                imports.insert(name.to_string(), full_path);
                            }
                        }
                    }
                }
            }
            None => {}
        }
    }
    imports
}

fn resolve_non_rust_call_name(
    call: &str,
    module_path: &str,
    import_map: &std::collections::HashMap<String, String>,
) -> String {
    if let Some(mapped) = import_map.get(call) {
        return mapped.clone();
    }
    if call.contains(".") {
        let segments = call.split('.').collect::<Vec<_>>();
        return format!("{}::{}", module_path, segments.last().unwrap_or(&call));
    }
    format!("{module_path}::{call}")
}

#[derive(Debug, Clone, Copy)]
enum Language {
    Rust,
    Python,
    JsLike,
}

impl From<NonRustLanguage> for Language {
    fn from(language: NonRustLanguage) -> Self {
        match language {
            NonRustLanguage::Python => Language::Python,
            NonRustLanguage::JsLike => Language::JsLike,
        }
    }
}

fn extract_doc_comments(content: &str, start_line: usize, language: Language) -> Option<String> {
    let lines = content.lines().collect::<Vec<_>>();
    if start_line == 0 || start_line > lines.len() {
        return None;
    }
    let mut doc_lines = Vec::new();
    for index in (0..start_line - 1).rev() {
        let trimmed = lines[index].trim_start();
        match language {
            Language::Rust => {
                if trimmed.starts_with("///") {
                    doc_lines.push(trimmed.trim_start_matches("///").trim());
                } else if trimmed.starts_with("//!" ) {
                    doc_lines.push(trimmed.trim_start_matches("//!" ).trim());
                } else if trimmed.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
            Language::Python => {
                if trimmed.starts_with('#') {
                    doc_lines.push(trimmed.trim_start_matches('#').trim());
                } else if trimmed.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
            Language::JsLike => {
                if trimmed.starts_with("//") {
                    doc_lines.push(trimmed.trim_start_matches("//").trim());
                } else if trimmed.starts_with("/*") {
                    doc_lines.push(trimmed.trim_matches(&['/', '*'][..]).trim());
                } else if trimmed.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
        }
    }
    if doc_lines.is_empty() {
        None
    } else {
        doc_lines.reverse();
        Some(doc_lines.join(" "))
    }
}

fn summarize_behavior(
    name: &str,
    _signature: &str,
    doc_comment: Option<String>,
    calls: &[String],
) -> String {
    if let Some(doc) = doc_comment {
        return doc;
    }
    let friendly_name = name.replace('_', " ");
    if let Some(call) = calls.first() {
        format!("Handles {friendly_name} and delegates to {call} when work continues.")
    } else {
        format!("Handles {friendly_name} and completes its own unit of work.")
    }
}

#[derive(Debug, Clone, Copy)]
enum NonRustLanguage {
    Python,
    JsLike,
}

fn detect_non_rust_language(relative_path: &str) -> Option<NonRustLanguage> {
    match relative_path.rsplit('.').next() {
        Some("py") => Some(NonRustLanguage::Python),
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") => Some(NonRustLanguage::JsLike),
        _ => None,
    }
}

struct FunctionDefinition {
    name: String,
    body: String,
    span: Option<LineSpan>,
}

fn collect_non_rust_function_definitions(
    content: &str,
    language: Option<NonRustLanguage>,
) -> Vec<FunctionDefinition> {
    let mut definitions = Vec::new();
    let lines = content.lines().collect::<Vec<_>>();

    for (index, line) in lines.iter().enumerate() {
        if let Some(lang) = language {
            let trimmed = line.trim_start();
            let maybe_name = match lang {
                NonRustLanguage::Python => parse_python_function_definition(trimmed),
                NonRustLanguage::JsLike => parse_js_function_definition(trimmed),
            };

            if let Some(name) = maybe_name {
                let start_line = index + 1;
                let span = span_for_non_rust_function(&lines, start_line, lang);
                let end_line = span
                    .as_ref()
                    .map(|span| span.end_line)
                    .unwrap_or(start_line);
                let body = lines
                    .iter()
                    .enumerate()
                    .filter_map(|(line_index, line)| {
                        let line_number = line_index + 1;
                        if (start_line..=end_line).contains(&line_number) {
                            Some(*line)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                definitions.push(FunctionDefinition { name, body, span });
            }
        }
    }

    definitions
}

fn parse_python_function_definition(line: &str) -> Option<String> {
    if line.starts_with("def ") || line.starts_with("async def ") {
        line.split_whitespace()
            .nth(1)
            .and_then(|signature| signature.split('(').next())
            .map(|s| s.to_string())
    } else {
        None
    }
}

fn parse_js_function_definition(line: &str) -> Option<String> {
    if line.starts_with("function ") || line.starts_with("async function ") {
        let mut parts = line.split_whitespace();
        parts
            .nth(if line.starts_with("async") { 2 } else { 1 })
            .and_then(|name| name.split('(').next())
            .map(|s| s.to_string())
    } else if line.starts_with("const ")
        || line.starts_with("let ")
        || line.starts_with("var ")
        || line.starts_with("export const ")
        || line.starts_with("export let ")
        || line.starts_with("export var ")
    {
        if let Some(index) = line.find('=') {
            let left = &line[..index].trim_end();
            let parts = left.split_whitespace().collect::<Vec<_>>();
            if let Some(name) = parts.last() {
                return Some(name.to_string());
            }
        }
        None
    } else {
        None
    }
}

fn span_for_non_rust_function(
    lines: &[&str],
    start_line: usize,
    language: NonRustLanguage,
) -> Option<LineSpan> {
    let start_index = start_line.saturating_sub(1);
    if start_index >= lines.len() {
        return None;
    }

    match language {
        NonRustLanguage::Python => {
            let start_line_text = lines[start_index];
            let start_indent = start_line_text
                .chars()
                .take_while(|c| c.is_whitespace())
                .count();
            let mut end_line = start_line;
            for (index, line) in lines.iter().enumerate().skip(start_index + 1) {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let indent = line.chars().take_while(|c| c.is_whitespace()).count();
                if indent <= start_indent {
                    break;
                }
                end_line = index + 1;
            }
            Some(LineSpan {
                start_line,
                end_line,
            })
        }
        NonRustLanguage::JsLike => {
            let mut brace_depth = 0_i64;
            let mut found_open = false;
            let mut end_line = start_line;
            for (index, line) in lines.iter().enumerate().skip(start_index) {
                for ch in line.chars() {
                    match ch {
                        '{' => {
                            brace_depth += 1;
                            found_open = true;
                        }
                        '}' => brace_depth -= 1,
                        _ => {}
                    }
                }
                if found_open && brace_depth <= 0 {
                    end_line = index + 1;
                    return Some(LineSpan {
                        start_line,
                        end_line,
                    });
                }
            }
            Some(LineSpan {
                start_line,
                end_line,
            })
        }
    }
}

fn collect_non_rust_calls(content: &str) -> Vec<String> {
    let mut calls = HashSet::new();
    let reserved = [
        "if", "for", "while", "switch", "catch", "return", "const", "let", "var", "function",
        "async", "new", "match", "await", "import", "export", "class",
    ];

    let bytes = content.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'(' {
            let mut start = index;
            while start > 0 && is_identifier_char(bytes[start - 1]) {
                start -= 1;
            }
            if start < index {
                let name = &content[start..index];
                if !name.is_empty()
                    && !reserved.contains(&name)
                    && name.chars().next().unwrap().is_ascii_alphabetic()
                {
                    calls.insert(name.to_string());
                }
            }
        }
        index += 1;
    }

    let mut calls = calls.into_iter().collect::<Vec<_>>();
    calls.sort();
    calls
}

fn is_identifier_char(byte: u8) -> bool {
    (byte == b'_') || byte.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_file;

    #[test]
    fn module_path_from_relative_uses_file_structure() {
        assert_eq!(module_path_from_relative("lib.rs", true), "crate");
        assert_eq!(module_path_from_relative("src/foo.rs", true), "crate::src::foo");
        assert_eq!(module_path_from_relative("foo/mod.rs", true), "crate::foo");
        assert_eq!(module_path_from_relative("utils/helper.py", false), "crate::utils::helper");
    }

    #[test]
    fn collect_rust_imports_maps_names_to_paths() {
        let syntax = parse_file("use crate::foo::bar; use self::baz::qux as quux;").unwrap();
        let imports = collect_rust_imports(&syntax, "crate::root");
        assert_eq!(imports.get("bar").map(String::as_str), Some("crate::foo::bar"));
        assert_eq!(imports.get("quux").map(String::as_str), Some("crate::baz::qux"));
    }

    #[test]
    fn resolve_rust_call_name_uses_import_map_and_local_defs() {
        let mut import_map = std::collections::HashMap::new();
        import_map.insert("bar".to_string(), "crate::foo::bar".to_string());
        let local_defs = vec!["baz".to_string()];
        assert_eq!(resolve_rust_call_name("bar", "crate::root", &import_map, &local_defs), "crate::foo::bar");
        assert_eq!(resolve_rust_call_name("baz", "crate::root", &import_map, &local_defs), "crate::root::baz");
        assert_eq!(resolve_rust_call_name("other", "crate::root", &import_map, &local_defs), "crate::other");
    }
}

fn infer_side_effects(name: &str, content: &str) -> Vec<String> {
    let mut effects = HashSet::new();
    let text = format!(
        "{} {}",
        name.to_ascii_lowercase(),
        content.to_ascii_lowercase()
    );
    for term in [
        "save", "persist", "write", "send", "log", "update", "delete", "remove", "create", "open",
        "close", "fetch", "post", "put", "patch",
    ] {
        if text.contains(term) {
            effects.insert(term.to_string());
        }
    }
    let mut effects = effects.into_iter().collect::<Vec<_>>();
    effects.sort();
    effects
}

fn importance_for_name(name: &str) -> GraphNodeImportance {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("query") || normalized.contains("search") {
        GraphNodeImportance::High
    } else if normalized.contains("build")
        || normalized.contains("persist")
        || normalized.contains("send")
    {
        GraphNodeImportance::Medium
    } else {
        GraphNodeImportance::Low
    }
}

fn span_for_function(content: &str, function_name: &str) -> Option<LineSpan> {
    let mut start_line = None;
    let mut brace_depth = 0_i64;
    let mut saw_open_brace = false;

    for (index, line) in content.lines().enumerate() {
        if start_line.is_none() && line.contains(&format!("fn {function_name}")) {
            start_line = Some(index + 1);
        }

        if start_line.is_some() {
            for ch in line.chars() {
                match ch {
                    '{' => {
                        brace_depth += 1;
                        saw_open_brace = true;
                    }
                    '}' => brace_depth -= 1,
                    _ => {}
                }
            }

            if saw_open_brace && brace_depth <= 0 {
                return Some(LineSpan {
                    start_line: start_line?,
                    end_line: index + 1,
                });
            }
        }
    }

    let start_line = start_line?;
    Some(LineSpan {
        start_line,
        end_line: start_line,
    })
}

fn relative_path_string(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("failed to compute relative path for {}", path.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn sha1_hex(bytes: &[u8]) -> String {
    let mut digest = Sha1::new();
    digest.update(bytes);
    format!("{:x}", digest.finalize())
}

fn collect_calls(block: &syn::Block) -> Vec<String> {
    let mut visitor = CallCollector { calls: Vec::new() };
    visitor.visit_block(block);
    visitor.calls.sort();
    visitor.calls.dedup();
    visitor.calls
}

struct CallCollector {
    calls: Vec<String>,
}

impl<'ast> Visit<'ast> for CallCollector {
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func
            && let Some(segment) = path.path.segments.last()
        {
            self.calls.push(segment.ident.to_string());
        }
        syn::visit::visit_expr_call(self, node);
    }
}
