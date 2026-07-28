//! Provided: end-to-end DST demo — requires dst.rs and shrink.rs
//! implemented. Finds each injected bug, then shrinks the failing
//! case to a minimal reproducer and prints it.

use testing_experiments::dst::find_bug;
use testing_experiments::kv::Bug;
use testing_experiments::shrink::shrink;

fn main() {
    // find_bug + shrink are both yours (src/dst.rs, src/shrink.rs), so this
    // binary has nothing of its own to print — probe once and say so.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let implemented = std::panic::catch_unwind(|| find_bug(Bug::LostDelete, 1, 4)).is_ok();
    std::panic::set_hook(prev);
    if !implemented {
        println!(
            "[stub — implement src/dst.rs and src/shrink.rs to unlock the DST run]\n\n\
             This binary searches seeds for each planted bug, then shrinks the failing\n\
             op sequence to a minimal repro. crash_matrix is the provided answer key:\n\
             run it first for the detection rates your dst.rs should roughly match."
        );
        return;
    }

    for bug in [Bug::LostDelete, Bug::NoSyncOnCommit, Bug::TornWriteAccepted, Bug::StaleRead] {
        match find_bug(bug, 500, 40) {
            None => println!("{bug:?}: NOT FOUND in 500 seeds (harness too weak?)"),
            Some(f) => {
                let small = shrink(&f.ops, f.seed, bug);
                println!("{bug:?}: seed {} failed at step {} ({} ops)", f.seed, f.step, f.ops.len());
                println!("  minimal repro ({} ops): {:?}\n", small.len(), small);
            }
        }
    }
}
