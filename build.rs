fn main() {
    println!("cargo:rustc-link-lib=sfuzzer");
    println!("cargo:rustc-link-lib=stdc++");
    println!("cargo:rustc-linker=clang");
}
