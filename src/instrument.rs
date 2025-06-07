use quote::ToTokens;
use std::error::Error;
use std::fmt::Display;
use std::fs;
use std::path::{Path as StdPath, PathBuf};
use syn::visit_mut::VisitMut;
use syn::{Block, Expr, ExprCall, ItemEnum, Path, Stmt, parse_quote};
use walkdir::WalkDir;

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
                write!(f, "Usage: {name} <path-to-rust-files>")
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

/// Visitor that instruments enum assignments
struct EnumInstrumenter {
    // Track enum types we've seen
    enum_types: std::collections::HashSet<String>,
    // Track enum variants and assign them unique IDs
    enum_variants: std::collections::HashMap<String, u32>,
    // Counter for generating unique location IDs
    location_counter: u32,
    // Track if we're in a const context
    in_const_context: bool,
}

impl EnumInstrumenter {
    fn new() -> Self {
        Self {
            enum_types: std::collections::HashSet::new(),
            enum_variants: std::collections::HashMap::new(),
            location_counter: 1, // Start from 1
            in_const_context: false,
        }
    }

    /// Generate the instrumentation call (only if not in const context)
    fn create_instrumentation_call(&mut self, enum_name: &str, variant_name: &str) -> Option<Stmt> {
        if self.in_const_context {
            return None; // Skip instrumentation in const contexts
        }

        let location = self.location_counter;
        self.location_counter += 1;

        // Create a unique key for this enum variant
        let variant_key = format!("{enum_name}::{variant_name}");

        // Assign a unique state value if we haven't seen this variant before
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

    /// Extract enum type and variant from a path expression
    fn extract_enum_info(&self, path: &Path) -> Option<(String, String)> {
        if path.segments.len() >= 2 {
            let enum_type = path.segments[path.segments.len() - 2].ident.to_string();
            let variant = path.segments.last()?.ident.to_string();

            // Only instrument if we know this is an enum type
            if self.enum_types.contains(&enum_type) {
                return Some((enum_type, variant));
            }
        }
        None
    }
}

impl VisitMut for EnumInstrumenter {
    /// Collect enum definitions
    fn visit_item_enum_mut(&mut self, node: &mut ItemEnum) {
        self.enum_types.insert(node.ident.to_string());
        syn::visit_mut::visit_item_enum_mut(self, node);
    }

    /// Track const functions
    fn visit_item_fn_mut(&mut self, node: &mut syn::ItemFn) {
        let was_const = self.in_const_context;
        if node.sig.constness.is_some() {
            self.in_const_context = true;
        }

        syn::visit_mut::visit_item_fn_mut(self, node);

        self.in_const_context = was_const;
    }

    /// Track const items
    fn visit_item_const_mut(&mut self, node: &mut syn::ItemConst) {
        let was_const = self.in_const_context;
        self.in_const_context = true;

        syn::visit_mut::visit_item_const_mut(self, node);

        self.in_const_context = was_const;
    }

    /// Track static items
    fn visit_item_static_mut(&mut self, node: &mut syn::ItemStatic) {
        let was_const = self.in_const_context;
        self.in_const_context = true;

        syn::visit_mut::visit_item_static_mut(self, node);

        self.in_const_context = was_const;
    }

    /// Instrument assignments in blocks
    fn visit_block_mut(&mut self, node: &mut Block) {
        let mut new_stmts = Vec::new();

        for stmt in &node.stmts {
            match stmt {
                // Handle let bindings with enum values
                Stmt::Local(local) => {
                    if let Some(init) = &local.init
                        && let Expr::Path(expr_path) = &*init.expr
                        && let Some((enum_name, variant_name)) =
                            self.extract_enum_info(&expr_path.path)
                    {
                        // Add instrumentation before the assignment
                        if let Some(instrumentation) =
                            self.create_instrumentation_call(&enum_name, &variant_name)
                        {
                            new_stmts.push(instrumentation);
                        }
                    }
                    new_stmts.push(stmt.clone());
                }

                // Handle expression statements that might be assignments
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

    /// Instrument function call arguments
    fn visit_expr_call_mut(&mut self, node: &mut ExprCall) {
        // Check if any arguments are enum variants
        for arg in &mut node.args {
            if let Expr::Path(expr_path) = arg &&
                let Some((enum_name, variant_name)) = self.extract_enum_info(&expr_path.path) &&
                    // For function arguments, we need a different approach
                    // We could wrap the argument in a block expression
                    let Some(instrumentation) =
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
}

/// Process a single Rust file
fn process_file(
    instrumenter: &mut EnumInstrumenter,
    file_path: &StdPath,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = fs::read_to_string(file_path)?;
    let mut syntax_tree = syn::parse_file(&content)?;

    // Apply instrumentation
    instrumenter.visit_file_mut(&mut syntax_tree);

    // Write back the modified code
    let output = syntax_tree.to_token_stream().to_string();

    // Write back to the same file (overwrite)
    fs::write(file_path, output)?;

    println!("Processed: {}", file_path.display());
    Ok(())
}

/// Process all Rust files in a directory
fn process_directory(
    instrumenter: &mut EnumInstrumenter,
    dir_path: &StdPath,
) -> Result<(), Box<dyn std::error::Error>> {
    for entry in WalkDir::new(dir_path) {
        let entry = entry?;
        if entry.file_type().is_file()
            && let Some(extension) = entry.path().extension()
            && extension == "rs"
            && let Err(e) = process_file(instrumenter, entry.path())
        {
            return Err(InstrumentError::ErrorProcessing(entry.path().to_owned(), e).into());
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() != 2 {
        return Err(InstrumentError::WrongArguments(args[0].to_string()).into());
    }

    let mut instrumenter = EnumInstrumenter::new();

    let path = StdPath::new(&args[1]);

    if path.is_file() {
        process_file(&mut instrumenter, path)?;
    } else if path.is_dir() {
        process_directory(&mut instrumenter, path)?;
    } else {
        return Err(InstrumentError::InvalidPath(path.to_owned()).into());
    }

    println!("Instrumentation complete!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enum_instrumentation() {
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

        let mut syntax_tree = syn::parse_file(input).unwrap();
        let mut instrumenter = EnumInstrumenter::new();
        instrumenter.visit_file_mut(&mut syntax_tree);

        let output = syntax_tree.to_token_stream().to_string();

        // Verify that instrumentation calls were added
        assert!(output.contains("sginstrument :: instrument (1u32 , 0u32)"));
        println!("Instrumented code:\n{}", output);
    }
}
