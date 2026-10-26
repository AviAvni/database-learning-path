//! Locate a SuiteSparse:GraphBLAS install and, if one is found, compile the C shim
//! (`shim/gb_shim.c`) against it and set `cfg(have_graphblas)`.
//!
//! When no install is found the crate still builds: the `gb_container` lane prints a
//! `[skip — libgraphblas not found]` note and exits 0, so `./verify.sh` works on a
//! machine that has never built GraphBLAS. To enable the lane, point GRAPHBLAS_DIR at
//! an install prefix with `include/[suitesparse/]GraphBLAS.h` and `lib/libgraphblas.*`
//! (a `cmake --install` prefix of a v10 build; FalkorDB's native-deps cache under
//! ~/.cache/falkordb/native-deps/graphblas/<hash>/ has the right shape and is
//! searched automatically).
use std::path::{Path, PathBuf};
use std::process::Command;

fn header_dir(prefix: &Path) -> Option<PathBuf> {
    for sub in ["include/suitesparse", "include"] {
        let d = prefix.join(sub);
        if d.join("GraphBLAS.h").exists() {
            return Some(d);
        }
    }
    None
}

fn has_lib(prefix: &Path) -> bool {
    ["a", "dylib", "so"]
        .iter()
        .any(|e| prefix.join("lib").join(format!("libgraphblas.{e}")).exists())
}

fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("GRAPHBLAS_DIR") {
        v.push(PathBuf::from(d));
        return v; // an explicit choice is not second-guessed
    }
    v.push(PathBuf::from("/usr/local"));
    v.push(PathBuf::from("/opt/homebrew"));
    v.push(PathBuf::from("/usr"));
    if let Ok(home) = std::env::var("HOME") {
        let cache = Path::new(&home).join(".cache/falkordb/native-deps/graphblas");
        if let Ok(rd) = std::fs::read_dir(&cache) {
            let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            // newest first, so the freshest build wins
            dirs.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
            dirs.reverse();
            v.extend(dirs);
        }
    }
    v
}

fn main() {
    println!("cargo:rustc-check-cfg=cfg(have_graphblas)");
    println!("cargo:rerun-if-changed=shim/gb_shim.c");
    println!("cargo:rerun-if-env-changed=GRAPHBLAS_DIR");
    let Some((prefix, inc)) = candidates()
        .into_iter()
        .find_map(|p| if has_lib(&p) { header_dir(&p).map(|h| (p, h)) } else { None })
    else {
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let obj = out.join("gb_shim.o");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let ok = Command::new(&cc)
        .args(["-O2", "-c", "shim/gb_shim.c", "-DGB_STATIC", "-I"])
        .arg(&inc)
        .arg("-o")
        .arg(&obj)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        println!(
            "cargo:warning=gb_shim.c did not compile against {}; gb_container lane disabled",
            inc.display()
        );
        return;
    }
    let lib = out.join("libgbshim.a");
    let ar_ok = Command::new("ar")
        .arg("rcs")
        .arg(&lib)
        .arg(&obj)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ar_ok {
        return;
    }
    println!("cargo:rustc-cfg=have_graphblas");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=gbshim");
    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-lib=graphblas");
    println!("cargo:rustc-env=GRAPHBLAS_PREFIX={}", prefix.display());
    // GraphBLAS is built with OpenMP; the static archive needs the runtime named.
    if cfg!(target_os = "macos") {
        for d in ["/opt/homebrew/opt/libomp/lib", "/usr/local/opt/libomp/lib"] {
            if Path::new(d).join("libomp.dylib").exists() {
                println!("cargo:rustc-link-search=native={d}");
                println!("cargo:rustc-link-arg=-Wl,-rpath,{d}");
                break;
            }
        }
        println!("cargo:rustc-link-lib=omp");
    } else {
        println!("cargo:rustc-link-lib=gomp");
        println!("cargo:rustc-link-lib=dl");
        println!("cargo:rustc-link-lib=m");
    }
}
