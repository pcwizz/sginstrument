use quote::ToTokens;
use std::error::Error;
use std::fmt::Display;
use std::fs;
use std::path::{Path as StdPath, PathBuf};
use syn::visit_mut::VisitMut;
use syn::{Block, Expr, ExprCall, ItemEnum, Pat, Path, Stmt, parse_quote};
use walkdir::WalkDir;

struct Config {
    dry_run: bool,
    backup: bool,
    path: PathBuf,
}

#[derive(Debug)]
enum InstrumentError {
    WrongArguments(String),
    ErrorProcessing(PathBuf, Box<dyn Error>),
    InvalidPath(PathBuf),
}

impl Display for InstrumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongArguments(name) => {
                writeln!(f, "Usage: {name} [OPTIONS] <path-to-rust-files>")?;
                writeln!(f)?;
                writeln!(f, "Options:")?;
                writeln!(f, "  --dry-run  Preview changes without modifying files")?;
                writeln!(f, "  --backup   Create .rs.bak files before overwriting")?;
                write!(f, "  -h, --help Show this help message")
            }
            Self::ErrorProcessing(path, error) => {
                let path = path.display();
                write!(f, "Error processing {path}: {error}")
            }
            Self::InvalidPath(path) => {
                let path = path.display();
                write!(f, "Invalid path: {path}")
            }
        }
    }
}

impl Error for InstrumentError {}

/// Visitor that instruments enum usage sites for SGFuzz state tracking.
struct EnumInstrumenter {
    enum_types: std::collections::HashSet<String>,
    enum_variants: std::collections::HashMap<String, u32>,
    location_counter: u32,
    in_const_context: bool,
}

impl EnumInstrumenter {
    fn new() -> Self {
        Self {
            enum_types: std::collections::HashSet::new(),
            enum_variants: std::collections::HashMap::new(),
            location_counter: 1,
            in_const_context: false,
        }
    }

    /// Generate the instrumentation call (only if not in const context).
    fn create_instrumentation_call(&mut self, enum_name: &str, variant_name: &str) -> Option<Stmt> {
        if self.in_const_context {
            return None;
        }

        let location = self.location_counter;
        self.location_counter += 1;

        let variant_key = format!("{enum_name}::{variant_name}");

        let next_state_value = self.enum_variants.len() as u32;
        let state_value = *self
            .enum_variants
            .entry(variant_key)
            .or_insert(next_state_value);

        let call = parse_quote! {
            sginstrument::instrument(#location, #state_value);
        };
        Some(call)
    }

    /// Extract enum type and variant from a path expression.
    fn extract_enum_info(&self, path: &Path) -> Option<(String, String)> {
        if path.segments.len() >= 2 {
            let enum_type = path.segments[path.segments.len() - 2].ident.to_string();
            let variant = path.segments.last()?.ident.to_string();

            if self.enum_types.contains(&enum_type) {
                return Some((enum_type, variant));
            }
        }
        None
    }

    /// Extract enum type and variant from a pattern (for match/if-let/while-let).
    fn extract_enum_info_from_pat(&self, pat: &Pat) -> Option<(String, String)> {
        match pat {
            Pat::Path(pat_path) => self.extract_enum_info(&pat_path.path),
            Pat::TupleStruct(pat_tuple) => self.extract_enum_info(&pat_tuple.path),
            Pat::Struct(pat_struct) => self.extract_enum_info(&pat_struct.path),
            _ => None,
        }
    }
}

impl VisitMut for EnumInstrumenter {
    /// Collect enum definitions.
    fn visit_item_enum_mut(&mut self, node: &mut ItemEnum) {
        self.enum_types.insert(node.ident.to_string());
        syn::visit_mut::visit_item_enum_mut(self, node);
    }

    /// Track const functions.
    fn visit_item_fn_mut(&mut self, node: &mut syn::ItemFn) {
        let was_const = self.in_const_context;
        if node.sig.constness.is_some() {
            self.in_const_context = true;
        }
        syn::visit_mut::visit_item_fn_mut(self, node);
        self.in_const_context = was_const;
    }

    /// Track const items.
    fn visit_item_const_mut(&mut self, node: &mut syn::ItemConst) {
        let was_const = self.in_const_context;
        self.in_const_context = true;
        syn::visit_mut::visit_item_const_mut(self, node);
        self.in_const_context = was_const;
    }

    /// Track static items.
    fn visit_item_static_mut(&mut self, node: &mut syn::ItemStatic) {
        let was_const = self.in_const_context;
        self.in_const_context = true;
        syn::visit_mut::visit_item_static_mut(self, node);
        self.in_const_context = was_const;
    }

    /// Instrument let-bindings and assignments in blocks.
    fn visit_block_mut(&mut self, node: &mut Block) {
        let mut new_stmts = Vec::new();

        for stmt in &node.stmts {
            match stmt {
                Stmt::Local(local) => {
                    if let Some(init) = &local.init
                        && let Expr::Path(expr_path) = &*init.expr
                        && let Some((enum_name, variant_name)) =
                            self.extract_enum_info(&expr_path.path)
                    {
                        if let Some(instrumentation) =
                            self.create_instrumentation_call(&enum_name, &variant_name)
                        {
                            new_stmts.push(instrumentation);
                        }
                    }
                    new_stmts.push(stmt.clone());
                }

                Stmt::Expr(expr, semi) => match expr {
                    Expr::Assign(assign) => {
                        if let Expr::Path(expr_path) = &*assign.right
                            && let Some((enum_name, variant_name)) =
                                self.extract_enum_info(&expr_path.path)
                            && let Some(instrumentation) =
                                self.create_instrumentation_call(&enum_name, &variant_name)
                        {
                            new_stmts.push(instrumentation);
                        }

                        new_stmts.push(Stmt::Expr(expr.clone(), *semi));
                    }
                    _ => new_stmts.push(stmt.clone()),
                },

                _ => new_stmts.push(stmt.clone()),
            }
        }

        node.stmts = new_stmts;
        syn::visit_mut::visit_block_mut(self, node);
    }

    /// Instrument function call arguments containing enum variants.
    fn visit_expr_call_mut(&mut self, node: &mut ExprCall) {
        for arg in &mut node.args {
            if let Expr::Path(expr_path) = arg
                && let Some((enum_name, variant_name)) =
                    self.extract_enum_info(&expr_path.path)
                && let Some(instrumentation) =
                    self.create_instrumentation_call(&enum_name, &variant_name)
            {
                let original_arg = arg.clone();

                *arg = parse_quote! {
                    {
                        #instrumentation
                        #original_arg
                    }
                };
            }
        }
        syn::visit_mut::visit_expr_call_mut(self, node);
    }

    /// Instrument method call arguments containing enum variants.
    fn visit_expr_method_call_mut(&mut self, node: &mut syn::ExprMethodCall) {
        for arg in &mut node.args {
            if let Expr::Path(expr_path) = arg
                && let Some((enum_name, variant_name)) =
                    self.extract_enum_info(&expr_path.path)
                && let Some(instrumentation) =
                    self.create_instrumentation_call(&enum_name, &variant_name)
            {
                let original_arg = arg.clone();

                *arg = parse_quote! {
                    {
                        #instrumentation
                        #original_arg
                    }
                };
            }
        }
        syn::visit_mut::visit_expr_method_call_mut(self, node);
    }

    /// Instrument match arms that pattern-match on enum variants.
    fn visit_expr_match_mut(&mut self, node: &mut syn::ExprMatch) {
        for arm in &mut node.arms {
            if let Some((enum_name, variant_name)) = self.extract_enum_info_from_pat(&arm.pat)
                && let Some(instrumentation) =
                    self.create_instrumentation_call(&enum_name, &variant_name)
            {
                let original_body = arm.body.clone();
                arm.body = Box::new(parse_quote! {
                    {
                        #instrumentation
                        #original_body
                    }
                });
            }
        }
        syn::visit_mut::visit_expr_match_mut(self, node);
    }

    /// Instrument `if let` expressions that destructure enum variants.
    fn visit_expr_if_mut(&mut self, node: &mut syn::ExprIf) {
        if let Expr::Let(expr_let) = &*node.cond
            && let Some((enum_name, variant_name)) =
                self.extract_enum_info_from_pat(&expr_let.pat)
            && let Some(instrumentation) =
                self.create_instrumentation_call(&enum_name, &variant_name)
        {
            node.then_branch.stmts.insert(0, instrumentation);
        }
        syn::visit_mut::visit_expr_if_mut(self, node);
    }

    /// Instrument `while let` expressions that destructure enum variants.
    fn visit_expr_while_mut(&mut self, node: &mut syn::ExprWhile) {
        if let Expr::Let(expr_let) = &*node.cond
            && let Some((enum_name, variant_name)) =
                self.extract_enum_info_from_pat(&expr_let.pat)
            && let Some(instrumentation) =
                self.create_instrumentation_call(&enum_name, &variant_name)
        {
            node.body.stmts.insert(0, instrumentation);
        }
        syn::visit_mut::visit_expr_while_mut(self, node);
    }
}

/// Process a single Rust file.
fn process_file(
    instrumenter: &mut EnumInstrumenter,
    file_path: &StdPath,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = fs::read_to_string(file_path)?;
    let mut syntax_tree = syn::parse_file(&content)?;

    let counter_before = instrumenter.location_counter;
    instrumenter.visit_file_mut(&mut syntax_tree);
    let points_added = instrumenter.location_counter - counter_before;

    let output = syntax_tree.to_token_stream().to_string();

    if config.dry_run {
        println!(
            "--- {} ({points_added} instrumentation points) ---",
            file_path.display()
        );
        println!("{output}");
        println!("--- end ---\n");
    } else {
        if config.backup {
            let backup_path = file_path.with_extension("rs.bak");
            fs::copy(file_path, &backup_path)?;
            println!("Backup: {}", backup_path.display());
        }
        fs::write(file_path, output)?;
        println!(
            "Processed: {} ({points_added} instrumentation points)",
            file_path.display()
        );
    }

    Ok(())
}

/// Process all Rust files in a directory.
fn process_directory(
    instrumenter: &mut EnumInstrumenter,
    dir_path: &StdPath,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in WalkDir::new(dir_path) {
        let entry = entry?;
        if entry.file_type().is_file()
            && let Some(extension) = entry.path().extension()
            && extension == "rs"
            && let Err(e) = process_file(instrumenter, entry.path(), config)
        {
            return Err(InstrumentError::ErrorProcessing(entry.path().to_owned(), e).into());
        }
    }
    Ok(())
}

fn parse_args() -> Result<Config, InstrumentError> {
    let args: Vec<String> = std::env::args().collect();
    let mut dry_run = false;
    let mut backup = false;
    let mut path = None;

    for arg in &args[1..] {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--backup" => backup = true,
            "--help" | "-h" => {
                println!("Usage: {} [OPTIONS] <path-to-rust-files>\n", args[0]);
                println!("Options:");
                println!("  --dry-run  Preview changes without modifying files");
                println!("  --backup   Create .rs.bak files before overwriting");
                println!("  -h, --help Show this help message");
                std::process::exit(0);
            }
            _ if arg.starts_with('-') => {
                return Err(InstrumentError::WrongArguments(args[0].to_string()));
            }
            _ => {
                if path.is_some() {
                    return Err(InstrumentError::WrongArguments(args[0].to_string()));
                }
                path = Some(PathBuf::from(arg));
            }
        }
    }

    let path = path.ok_or_else(|| InstrumentError::WrongArguments(args[0].to_string()))?;
    Ok(Config {
        dry_run,
        backup,
        path,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = parse_args()?;
    let mut instrumenter = EnumInstrumenter::new();

    let path = &config.path;
    if path.is_file() {
        process_file(&mut instrumenter, path, &config)?;
    } else if path.is_dir() {
        process_directory(&mut instrumenter, path, &config)?;
    } else {
        return Err(InstrumentError::InvalidPath(path.to_owned()).into());
    }

    let total_points = instrumenter.location_counter - 1;
    let unique_variants = instrumenter.enum_variants.len();
    println!("\nInstrumentation complete!");
    println!("  Total instrumentation points: {total_points}");
    println!("  Unique enum variants tracked: {unique_variants}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instrument_code(input: &str) -> String {
        let mut syntax_tree = syn::parse_file(input).unwrap();
        let mut instrumenter = EnumInstrumenter::new();
        instrumenter.visit_file_mut(&mut syntax_tree);
        syntax_tree.to_token_stream().to_string()
    }

    fn instrument_code_with_stats(input: &str) -> (String, EnumInstrumenter) {
        let mut syntax_tree = syn::parse_file(input).unwrap();
        let mut instrumenter = EnumInstrumenter::new();
        instrumenter.visit_file_mut(&mut syntax_tree);
        (syntax_tree.to_token_stream().to_string(), instrumenter)
    }

    #[test]
    fn test_let_binding_instrumentation() {
        let input = r#"
enum Status {
    Active,
    Inactive,
    Pending(i32),
}

fn main() {
    let status = Status::Active;
    let mut other = Status::Pending(42);
    other = Status::Inactive;

    process_status(Status::Active);
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "let-binding and assignment sites should be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_match_instrumentation() {
        let input = r#"
enum State {
    Running,
    Stopped,
    Paused(u32),
}

fn check(s: State) {
    match s {
        State::Running => println!("running"),
        State::Stopped => println!("stopped"),
        State::Paused(x) => println!("paused {}", x),
    }
}
"#;

        let (output, instrumenter) = instrument_code_with_stats(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "match arms should be instrumented, got:\n{output}"
        );
        assert_eq!(
            instrumenter.location_counter - 1,
            3,
            "3 match arms should produce 3 instrumentation points"
        );
    }

    #[test]
    fn test_if_let_instrumentation() {
        let input = r#"
enum Command {
    Start,
    Stop,
    Restart(u32),
}

fn handle(cmd: Command) {
    if let Command::Restart(delay) = cmd {
        println!("restarting in {}", delay);
    }
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "if-let should be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_while_let_instrumentation() {
        let input = r#"
enum Item {
    Value(i32),
    End,
}

fn drain(items: &mut Vec<Item>) {
    while let Item::Value(v) = items.remove(0) {
        println!("{}", v);
    }
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "while-let should be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_method_call_instrumentation() {
        let input = r#"
enum Color {
    Red,
    Blue,
}

struct Painter;
impl Painter {
    fn paint(&self, c: Color) {}
}

fn draw(p: Painter) {
    p.paint(Color::Red);
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "method call args should be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_match_struct_variant() {
        let input = r#"
enum Message {
    Quit,
    Move { x: i32, y: i32 },
}

fn handle(msg: Message) {
    match msg {
        Message::Quit => {},
        Message::Move { x, y } => println!("{} {}", x, y),
    }
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "struct variant patterns in match should be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_const_context_skipped() {
        let input = r#"
enum Mode {
    Fast,
    Slow,
}

const fn default_mode() -> Mode {
    Mode::Fast
}

const DEFAULT: Mode = Mode::Slow;
"#;

        let output = instrument_code(input);

        assert!(
            !output.contains("sginstrument :: instrument"),
            "const contexts should not be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_static_context_skipped() {
        let input = r#"
enum Level {
    High,
    Low,
}

static LEVEL: Level = Level::High;
"#;

        let output = instrument_code(input);

        assert!(
            !output.contains("sginstrument :: instrument"),
            "static contexts should not be instrumented, got:\n{output}"
        );
    }

    #[test]
    fn test_unknown_enum_not_instrumented() {
        let input = r#"
fn main() {
    let x = Unknown::Variant;
}
"#;

        let output = instrument_code(input);

        assert!(
            !output.contains("sginstrument :: instrument"),
            "unknown enums should not be instrumented"
        );
    }

    #[test]
    fn test_unique_variant_ids() {
        let input = r#"
enum Toggle {
    On,
    Off,
}

fn main() {
    let a = Toggle::On;
    let b = Toggle::Off;
    let c = Toggle::On;
}
"#;

        let (_, instrumenter) = instrument_code_with_stats(input);

        assert_eq!(
            instrumenter.enum_variants.len(),
            2,
            "On and Off should have distinct variant IDs"
        );
        assert_eq!(
            instrumenter.location_counter, 4,
            "3 assignments should produce 3 locations (counter starts at 1)"
        );
    }

    #[test]
    fn test_locations_are_unique() {
        let input = r#"
enum Signal {
    Start,
    Stop,
}

fn main() {
    let a = Signal::Start;
    let b = Signal::Start;
}
"#;

        let (output, instrumenter) = instrument_code_with_stats(input);

        assert!(output.contains("1u32"), "first location should be 1");
        assert!(output.contains("2u32"), "second location should be 2");
        assert_eq!(
            instrumenter.location_counter, 3,
            "2 instrumentation points, counter should be 3"
        );
    }

    #[test]
    fn test_fn_call_arg_instrumentation() {
        let input = r#"
enum Color {
    Red,
    Blue,
}

fn paint(c: Color) {}

fn main() {
    paint(Color::Red);
}
"#;

        let output = instrument_code(input);

        assert!(
            output.contains("sginstrument :: instrument"),
            "function call arguments should be instrumented, got:\n{output}"
        );
    }
}
