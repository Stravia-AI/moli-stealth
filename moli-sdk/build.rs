//! Production always uses the fixed GitHub binding and verified HTTPS transport.
mod build_loader;

fn main() {
    build_loader::main();
}
