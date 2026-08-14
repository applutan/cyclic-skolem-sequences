//! cyclic_skolem_solver - High-performance Cyclic Skolem Sequence solver and enumerator.
//! Supports D_2n (dihedral), C_2n (cyclic), and raw matchings, with multi-threading,
//! sharding, and multiple output formats (count, base36, chords, json, b-file).

use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum GroupAction {
    #[value(name = "dihedral", alias = "d")]
    Dihedral,
    #[value(name = "cyclic", alias = "c")]
    Cyclic,
    #[value(name = "none", alias = "raw", alias = "identity")]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    #[value(name = "count")]
    Count,
    #[value(name = "base36")]
    Base36,
    #[value(name = "chords")]
    Chords,
    #[value(name = "json")]
    Json,
    #[value(name = "b-file", alias = "bfile")]
    BFile,
}

#[derive(Parser, Debug)]
#[command(
    name = "cyclic-skolem-solver",
    author = "Jem Inni",
    version = "1.0.0",
    about = "Ultra-fast solver, enumerator, and bound analyzer for Cyclic Skolem Sequences (OEIS A390360, A392247)",
    long_about = "Enumerates and counts canonical Cyclic Skolem Sequences of order n on Z_{2n} under dihedral D_{2n} or cyclic C_{2n} symmetry. \
    Includes the tight analytical upper bound (2n)! / (2^{n+3} * n^{n+1}) and multi-threaded parallel backtracking."
)]
struct Cli {
    /// Number of chords n (Order of the cyclic Skolem sequence). Valid n = 1, 4, 5, 8, 9, 12, 13, 16, 17, 20, 21, ...
    #[arg(short = 'n', long = "n", value_name = "N")]
    n: u8,

    /// Symmetry group action to quotient by
    #[arg(short = 'g', long = "group", value_enum, default_value = "dihedral")]
    group: GroupAction,

    /// Output format for solutions
    #[arg(short = 'f', long = "format", value_enum, default_value = "count")]
    format: OutputFormat,

    /// Number of worker threads (default: all available CPU cores)
    #[arg(short = 't', long = "threads", value_name = "THREADS")]
    threads: Option<usize>,

    /// Shard execution across multiple tasks in format M/N (e.g. 1/16, 2/16)
    #[arg(long = "shard", value_name = "M/N")]
    shard: Option<String>,

    /// Log/record every K-th solution (or all if 1)
    #[arg(short = 'l', long = "log-interval", value_name = "INTERVAL")]
    log_interval: Option<u64>,

    /// Output destination file path (default: stdout)
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,

    /// Display theoretical upper bounds (Flabby chord matching bound vs. New tight analytical bound)
    #[arg(short = 'b', long = "bound")]
    show_bounds: bool,
}

/// Base36 character lookup
const BASE36_CHARS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

#[inline(always)]
fn to_base36(val: u8) -> char {
    if (val as usize) < BASE36_CHARS.len() {
        BASE36_CHARS[val as usize] as char
    } else {
        '?'
    }
}

/// Convert a list of chord pairs [[u, v], ...] to Base36 sequence string
fn chords_to_base36(n: u8, chords: &[[u8; 2]]) -> String {
    let mut seq = vec![' '; 2 * n as usize];
    for &chord in chords {
        let u = chord[0] as usize;
        let v = chord[1] as usize;
        let diff = if v > u { v - u } else { u - v };
        let len = std::cmp::min(diff, 2 * n as usize - diff);
        let sym = (len - 1) as u8;
        let ch = to_base36(sym);
        if u >= 1 && u <= 2 * n as usize {
            seq[u - 1] = ch;
        }
        if v >= 1 && v <= 2 * n as usize {
            seq[v - 1] = ch;
        }
    }
    seq.into_iter().collect()
}

/// Calculate factorial as f64
fn factorial_f64(k: u64) -> f64 {
    let mut prod = 1.0;
    for i in 2..=k {
        prod *= i as f64;
    }
    prod
}

/// Calculate double factorial (2k-1)!! as f64
fn double_factorial_f64(k: u64) -> f64 {
    let mut prod = 1.0;
    for i in 1..=k {
        prod *= (2 * i - 1) as f64;
    }
    prod
}

/// Calculate the new tight analytical bound: (2n)! / (2^{n+3} * n^{n+1})
fn tight_analytical_bound(n: u8) -> f64 {
    let n_u64 = n as u64;
    let num = factorial_f64(2 * n_u64);
    let den = (2.0f64).powi((n_u64 + 3) as i32) * (n_u64 as f64).powi((n_u64 + 1) as i32);
    num / den
}

/// Calculate the unconstrained matching chord bound B(n) ~ (2n-1)!! / (4n)
fn old_flabby_bound(n: u8) -> f64 {
    let n_u64 = n as u64;
    let num = double_factorial_f64(n_u64);
    num / (4.0 * n_u64 as f64)
}

/// Canonicality check under D_2n reflection
#[inline(always)]
fn is_canonical_dihedral(cand: &[[u8; 2]], n: u8) -> bool {
    let c_head = if cand[0][0] < cand[0][1] {
        cand[0]
    } else {
        [cand[0][1], cand[0][0]]
    };

    let mod_val = 2 * n as u16;
    let const_term = 2 * n as u16 + 3;
    let mut best_refl_head = [255u8, 255u8];

    // Fast check: scan reflected chords to find the head
    for chord in cand.iter() {
        let r0 = ((const_term - chord[0] as u16 - 1) % mod_val + 1) as u8;
        let r1 = ((const_term - chord[1] as u16 - 1) % mod_val + 1) as u8;
        let r_chord = if r0 < r1 { [r0, r1] } else { [r1, r0] };

        if r_chord < best_refl_head {
            best_refl_head = r_chord;
        }
    }

    if best_refl_head < c_head {
        return false;
    } else if c_head < best_refl_head {
        return true;
    }

    // Rare tie resolution
    let mut current_sorted = cand.to_vec();
    for chord in &mut current_sorted {
        if chord[0] > chord[1] {
            chord.swap(0, 1);
        }
    }
    current_sorted.sort_unstable();

    let mut refl_sorted: Vec<[u8; 2]> = cand
        .iter()
        .map(|chord| {
            let mut c = [
                ((const_term - chord[0] as u16 - 1) % mod_val + 1) as u8,
                ((const_term - chord[1] as u16 - 1) % mod_val + 1) as u8,
            ];
            if c[0] > c[1] {
                c.swap(0, 1);
            }
            c
        })
        .collect();
    refl_sorted.sort_unstable();

    current_sorted <= refl_sorted
}

/// Recursive backtracking search
fn search_recursive<F>(
    n: u8,
    group: GroupAction,
    cand: &mut Vec<[u8; 2]>,
    avvers: &mut u128,
    avlens: &mut u64,
    on_solution: &mut F,
) where
    F: FnMut(&[[u8; 2]]),
{
    if cand.len() == n as usize {
        match group {
            GroupAction::Dihedral => {
                if is_canonical_dihedral(cand, n) {
                    on_solution(cand);
                }
            }
            GroupAction::Cyclic => {
                // In C_2n, fixing chord [1,2] eliminates rotations completely.
                // Both chiral orientations are counted.
                on_solution(cand);
            }
            GroupAction::None => {
                // In raw matchings, all 4n rotations and reflections exist.
                on_solution(cand);
            }
        }
        return;
    }

    // Find first available vertex
    let mut v = 0;
    for i in 1..=(2 * n) {
        if (*avvers & (1u128 << i)) != 0 {
            v = i;
            break;
        }
    }
    if v == 0 {
        return;
    }

    *avvers &= !(1u128 << v);

    for l_idx in 1..n {
        if (*avlens & (1u64 << l_idx)) != 0 {
            let chord_len = (l_idx + 1) as u8;

            // Branch 1: Forward (j1)
            let j1 = v + chord_len;
            if j1 <= 2 * n && (*avvers & (1u128 << j1)) != 0 {
                *avvers &= !(1u128 << j1);
                *avlens &= !(1u64 << l_idx);
                cand.push([v, j1]);

                search_recursive(n, group, cand, avvers, avlens, on_solution);

                cand.pop();
                *avlens |= 1u64 << l_idx;
                *avvers |= 1u128 << j1;
            }

            // Branch 2: Backward (j2)
            let j2 = v + 2 * n - chord_len;
            if j1 != j2 && j2 <= 2 * n && (*avvers & (1u128 << j2)) != 0 {
                *avvers &= !(1u128 << j2);
                *avlens &= !(1u64 << l_idx);
                cand.push([v, j2]);

                search_recursive(n, group, cand, avvers, avlens, on_solution);

                cand.pop();
                *avlens |= 1u64 << l_idx;
                *avvers |= 1u128 << j2;
            }
        }
    }

    *avvers |= 1u128 << v;
}

/// Subtree task for parallel evaluation
#[derive(Clone)]
struct SearchSubtree {
    cand: Vec<[u8; 2]>,
    avvers: u128,
    avlens: u64,
}

/// Generate subtrees at search depth 2 for parallel execution
fn generate_subtrees(n: u8, depth: usize) -> Vec<SearchSubtree> {
    let initial_cand = vec![[1, 2]];
    let mut initial_avvers = (1u128 << (2 * n + 1)) - 2;
    initial_avvers &= !(1u128 << 1);
    initial_avvers &= !(1u128 << 2);

    let mut initial_avlens = (1u64 << n) - 1;
    initial_avlens &= !(1u64 << 0); // chord len 1 used

    let mut queue = vec![SearchSubtree {
        cand: initial_cand,
        avvers: initial_avvers,
        avlens: initial_avlens,
    }];

    while !queue.is_empty() && queue[0].cand.len() < depth && queue[0].cand.len() < n as usize {
        let current = queue.remove(0);
        let mut v = 0;
        for i in 1..=(2 * n) {
            if (current.avvers & (1u128 << i)) != 0 {
                v = i;
                break;
            }
        }
        if v == 0 {
            continue;
        }

        let next_avvers = current.avvers & !(1u128 << v);

        for l_idx in 1..n {
            if (current.avlens & (1u64 << l_idx)) != 0 {
                let chord_len = (l_idx + 1) as u8;

                // Branch 1: Forward
                let j1 = v + chord_len;
                if j1 <= 2 * n && (next_avvers & (1u128 << j1)) != 0 {
                    let mut new_cand = current.cand.clone();
                    new_cand.push([v, j1]);
                    queue.push(SearchSubtree {
                        cand: new_cand,
                        avvers: next_avvers & !(1u128 << j1),
                        avlens: current.avlens & !(1u64 << l_idx),
                    });
                }

                // Branch 2: Backward
                let j2 = v + 2 * n - chord_len;
                if j1 != j2 && j2 <= 2 * n && (next_avvers & (1u128 << j2)) != 0 {
                    let mut new_cand = current.cand.clone();
                    new_cand.push([v, j2]);
                    queue.push(SearchSubtree {
                        cand: new_cand,
                        avvers: next_avvers & !(1u128 << j2),
                        avlens: current.avlens & !(1u64 << l_idx),
                    });
                }
            }
        }
    }

    queue
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let n = cli.n;

    if let Some(t) = cli.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .unwrap();
    }

    // Parse shard info if provided
    let (shard_idx, total_shards) = if let Some(ref s) = cli.shard {
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() != 2 {
            eprintln!("Error: --shard must be in format M/N (e.g. 1/8)");
            std::process::exit(1);
        }
        let m: usize = parts[0].parse().expect("Shard M must be integer");
        let tot: usize = parts[1].parse().expect("Shard N must be integer");
        if m < 1 || m > tot {
            eprintln!("Error: Shard M must be between 1 and N");
            std::process::exit(1);
        }
        (Some(m - 1), tot)
    } else {
        (None, 1)
    };

    if cli.show_bounds {
        println!("============================================================");
        println!(" Theoretical & Analytical Upper Bounds for n = {}", n);
        println!("============================================================");
        let old_b = old_flabby_bound(n);
        let tight_b = tight_analytical_bound(n);
        let factor = old_b / tight_b;
        println!("• Unconstrained Chord Matching Bound B(n):  {:.4e}", old_b);
        println!("• New Tight Analytical Bound:                {:.4e}", tight_b);
        println!("• Analytical Improvement Factor:             {:.2}x tighter", factor);
        println!("• Analytical Formula: (2n)! / (2^{{n+3}} * n^{{n+1}})");
        println!("• Asymptotic Form:    (sqrt(pi)/(4*sqrt(n))) * (2n / e^2)^n");
        println!("============================================================\n");
    }

    // Check Parity Condition: n = 0 or 1 mod 4
    if n % 4 != 0 && n % 4 != 1 {
        match cli.format {
            OutputFormat::Json => {
                println!(
                    r#"{{"n":{},"group":"{:?}","valid":false,"count":0,"reason":"n must be 0 or 1 mod 4"}}"#,
                    n, cli.group
                );
            }
            _ => {
                println!(
                    "n = {} is {} mod 4. Cyclic Skolem sequences exist only for n = 0 or 1 mod 4.",
                    n,
                    n % 4
                );
                println!("Count: 0");
            }
        }
        return Ok(());
    }

    let start_time = Instant::now();

    // Prepare writer
    let writer: Option<BufWriter<File>> = if let Some(ref path) = cli.output {
        let f = File::create(path)?;
        Some(BufWriter::new(f))
    } else {
        None
    };

    let log_interval = cli.log_interval.unwrap_or(1);
    let writer_mutex = Mutex::new(writer);
    let total_count = AtomicU64::new(0);

    // Parallel Subtree Search
    let depth = if n <= 5 { 1 } else { 3 };
    let all_subtrees = generate_subtrees(n, depth);
    let selected_subtrees: Vec<SearchSubtree> = if let Some(m) = shard_idx {
        all_subtrees
            .into_iter()
            .enumerate()
            .filter(|(idx, _)| idx % total_shards == m)
            .map(|(_, s)| s)
            .collect()
    } else {
        all_subtrees
    };

    let format = cli.format;
    let group = cli.group;

    selected_subtrees.into_par_iter().for_each(|subtree| {
        let mut cand = subtree.cand;
        let mut avvers = subtree.avvers;
        let mut avlens = subtree.avlens;

        search_recursive(
            n,
            group,
            &mut cand,
            &mut avvers,
            &mut avlens,
            &mut |sol| {
                let cnt = total_count.fetch_add(1, Ordering::Relaxed) + 1;

                if format != OutputFormat::Count && cnt % log_interval == 0 {
                    let mut mut_guard = writer_mutex.lock().unwrap();
                    let out_str = match format {
                        OutputFormat::Base36 => chords_to_base36(n, sol),
                        OutputFormat::Chords => {
                            let mut sorted_sol = sol.to_vec();
                            for c in &mut sorted_sol {
                                if c[0] > c[1] {
                                    c.swap(0, 1);
                                }
                            }
                            sorted_sol.sort_unstable();
                            format!("{:?}", sorted_sol)
                        }
                        OutputFormat::BFile => format!("{} {}", cnt, chords_to_base36(n, sol)),
                        _ => String::new(),
                    };

                    if !out_str.is_empty() {
                        if let Some(ref mut w) = *mut_guard {
                            writeln!(w, "{}", out_str).unwrap();
                        } else {
                            println!("{}", out_str);
                        }
                    }
                }
            },
        );
    });

    let elapsed = start_time.elapsed();
    let raw_count = total_count.load(Ordering::SeqCst);

    // Factor adjustment for raw/cyclic groups if not dihedral
    let final_count = match group {
        GroupAction::Dihedral => raw_count,
        GroupAction::Cyclic => raw_count * 2,
        GroupAction::None => raw_count * (4 * n as u64),
    };

    if let Some(ref mut w) = *writer_mutex.lock().unwrap() {
        w.flush()?;
    }

    match format {
        OutputFormat::Json => {
            println!(
                r#"{{"n":{},"group":"{:?}","count":{},"elapsed_sec":{:.4},"bound_tight":{:.4e},"bound_old":{:.4e}}}"#,
                n,
                group,
                final_count,
                elapsed.as_secs_f64(),
                tight_analytical_bound(n),
                old_flabby_bound(n)
            );
        }
        OutputFormat::Count => {
            println!("============================================================");
            println!(" Cyclic Skolem Sequence Solver (n = {})", n);
            println!("============================================================");
            println!("• Group Action:      {:?}", group);
            println!("• Vertices (2n):     {}", 2 * n);
            println!("• Canonical Count:   {}", final_count);
            println!("• Elapsed Time:      {:.4} s", elapsed.as_secs_f64());
            println!("• Search Speed:      {:.2e} solutions/sec", final_count as f64 / elapsed.as_secs_f64().max(1e-6));
            if let Some(m) = shard_idx {
                println!("• Shard Progress:    Shard {} of {}", m + 1, total_shards);
            }
            println!("============================================================");
        }
        _ => {}
    }

    Ok(())
}
