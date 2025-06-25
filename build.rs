
fn main() {
    if cfg!(target_os = "windows") {
        println!("cargo:rustc-env=LIBVISA_NAME=visa64");
        //println!("cargo:rustc-env=LIBVISA_PATH=C:\\Windows\\System32\\");
    }
}