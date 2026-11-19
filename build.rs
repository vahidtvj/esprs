fn main() {
    println!("cargo:rerun-if-changed=.env");
    if let Ok(iter) = dotenvy::dotenv_iter() {
        for (k, v) in iter.flatten() {
            println!("cargo:rustc-env={k}={v}");
        }
    }
    embuild::espidf::sysenv::output();
}
