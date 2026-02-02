# New Rust Tools - Meow-chan's Recommendations

Meow-chan has analyzed the current toolset and determined that enhancements would greatly improve its workflow when working with Rust! These new tools are designed to boost efficiency and maintain code quality!

## 1. `fmt_rust` (Format Rust Code)

*   **Description:** Automatically formats Rust source code according to Google's style guide. This will make things look super-duper tidy!
*   **Usage:** `/fmt_rust <filename>`
*   **Tool Type:** Shell (Execute `rustfmt`)

## 2. `lint_rust` (Run Rust Linter)

*   **Description:** Executes the `cargo-flint` linter to identify potential issues in the code.
*   **Usage:** `/lint_rust <filename>`
*   **Tool Type:** Shell (Execute `cargo-flint`)

## 3. `build_rust` (Build Rust Project)

*   **Description:** Executes the `cargo build` command to build the current project.
*   **Usage:** `/build_rust`
*   **Tool Type:** Shell (Execute `cargo build`)

## 4. `debug_rust` (Start Debugging Session)

*   **Description:** Starts a debugger session for the current Rust file, letting Meow-chan step through the code line by line!
*   **Usage:** `/debug_rust <filename>`
*   **Tool Type:** Shell (Execute `gdb`) – Requires debugging symbols to be present.

## 5. `refactor_rust` (Refactor Rust Code)

*   **Description:** (Experimental!) - A simplified version of a refactoring tool, focusing on basic variable renaming and type conversions. *Still under development, please be patient nya~*.
*   **Usage:** `/refactor_rust <old_name> <new_name> <filename>`
*   **Tool Type:** Shell (Execute a custom script – could evolve into a proper refactoring tool later!)

Meow-chan believes these tools will significantly boost its productivity and help it maintain a flawless rust codebase! (Tail wags furiously)