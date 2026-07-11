use fulltext_experiments::{bm25, corpus, index, postings, wand};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Instant;

fn time_ms(f: impl FnOnce()) -> f64 {
    let t = Instant::now();
    f();
    t.elapsed().as_secs_f64() * 1e3
}

fn main() {
    println!("== corpus + index build ==");
    let t = Instant::now();
    let c = corpus::gen_corpus(100_000, 50_000, 1.0, 42);
    let gen_ms = t.elapsed().as_secs_f64() * 1e3;
    let tokens: usize = c.docs.iter().map(|d| d.len()).sum();
    let t = Instant::now();
    let idx = index::build_index(&c);
    let build_ms = t.elapsed().as_secs_f64() * 1e3;
    let n_postings: usize = idx.lists.iter().map(|l| l.postings.len()).sum();
    println!(
        "docs 100000  tokens {tokens}  postings {n_postings}  gen {gen_ms:.0} ms  build {build_ms:.0} ms"
    );
    println!(
        "df(t0)={}  df(t100)={}  df(t10000)={}  avg_len={:.1}",
        idx.lists[0].postings.len(),
        idx.lists[100].postings.len(),
        idx.lists[10_000].postings.len(),
        idx.avg_len
    );

    println!("\n== BM25 top-10: oracle (term-at-a-time, exhaustive) ==");
    let queries: &[(&str, Vec<u32>)] = &[
        ("common∧common   [t0 t1 t5]", vec![0, 1, 5]),
        ("mid∧mid         [t100 t1000]", vec![100, 1000]),
        ("common∧rare     [t0 t12000]", vec![0, 12_000]),
        ("rare∧rare       [t9000 t15000]", vec![9_000, 15_000]),
    ];
    for (name, q) in queries {
        let (top, work) = bm25::oracle_topk(&idx, q, 10);
        let reps = 20;
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(bm25::oracle_topk(&idx, std::hint::black_box(q), 10));
        }
        let ms = t.elapsed().as_secs_f64() * 1e3 / reps as f64;
        println!(
            "{name}  {ms:8.3} ms  postings {work:>7}  top1 ({:.3}, doc {})",
            top[0].0, top[0].1
        );
    }

    println!("\n== block-max WAND (stub lane) ==");
    for (name, q) in queries {
        let r = catch_unwind(AssertUnwindSafe(|| {
            let (top, stats) = wand::wand_topk(&idx, q, 10);
            let reps = 20;
            let t = Instant::now();
            for _ in 0..reps {
                std::hint::black_box(wand::wand_topk(&idx, std::hint::black_box(q), 10));
            }
            let ms = t.elapsed().as_secs_f64() * 1e3 / reps as f64;
            println!(
                "{name}  {ms:8.3} ms  scored {:>7}  skipped {:>8}  top1 ({:.3}, doc {})",
                stats.docs_scored, stats.postings_skipped, top[0].0, top[0].1
            );
        }));
        if r.is_err() {
            println!("{name}  [stub — implement wand::wand_topk]");
            break;
        }
    }

    println!("\n== posting-list AND/OR: sorted vec vs roaring ==");
    let a: Vec<u32> = idx.lists[0].postings.iter().map(|p| p.doc).collect();
    let b: Vec<u32> = idx.lists[1].postings.iter().map(|p| p.doc).collect();
    let s: Vec<u32> = idx.lists[5_000].postings.iter().map(|p| p.doc).collect();
    println!("|t0|={}  |t1|={}  |t5000|={}", a.len(), b.len(), s.len());
    let pairs: &[(&str, &[u32], &[u32])] =
        &[("dense∧dense  t0,t1", &a, &b), ("dense∧sparse t0,t5000", &a, &s)];
    for (name, x, y) in pairs {
        let reps = 200;
        let and_ms = time_ms(|| {
            for _ in 0..reps {
                std::hint::black_box(postings::vec_and(
                    std::hint::black_box(x),
                    std::hint::black_box(y),
                ));
            }
        }) / reps as f64;
        let or_ms = time_ms(|| {
            for _ in 0..reps {
                std::hint::black_box(postings::vec_or(
                    std::hint::black_box(x),
                    std::hint::black_box(y),
                ));
            }
        }) / reps as f64;
        println!("vec     {name}  and {and_ms:.4} ms  or {or_ms:.4} ms  |and|={}", postings::vec_and(x, y).len());
        let r = catch_unwind(AssertUnwindSafe(|| {
            let (rx, ry) = (
                postings::Roaring::from_sorted(x),
                postings::Roaring::from_sorted(y),
            );
            let and_ms = time_ms(|| {
                for _ in 0..reps {
                    std::hint::black_box(std::hint::black_box(&rx).and(std::hint::black_box(&ry)));
                }
            }) / reps as f64;
            let or_ms = time_ms(|| {
                for _ in 0..reps {
                    std::hint::black_box(std::hint::black_box(&rx).or(std::hint::black_box(&ry)));
                }
            }) / reps as f64;
            println!("roaring {name}  and {and_ms:.4} ms  or {or_ms:.4} ms");
        }));
        if r.is_err() {
            println!("roaring {name}  [stub — implement postings::Roaring]");
        }
    }
}
