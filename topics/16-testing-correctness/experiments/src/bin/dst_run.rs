//! Provided: end-to-end DST demo — requires dst.rs and shrink.rs
//! implemented. Finds each injected bug, then shrinks the failing
//! case to a minimal reproducer and prints it.

use testing_experiments::dst::find_bug;
use testing_experiments::kv::Bug;
use testing_experiments::shrink::shrink;

fn main() {
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
