//! HIR 端到端冒烟：内存文件系统里喂一段源码，打印降级结果。
//!
//! 运行：`cargo run -p novella-hir --example hir_smoke`

use std::io;
use std::path::Path;

use novella_sema::{FileLoader, analyze, analyze_with};

/// 固定文件名：`analyze_with` 的 crate 根查找也走这里，故只看一个虚拟文件。
const ENTRY: &str = "/virtual/smoke.nov";

const SOURCE: &str = r#"func main() {
    let result: i32 = sum(10);
    let more: i32 = coverage();
}

func add(a: i32, b: i32 = a) -> i32 {
    return a + b;
}

func classify(n: i32) -> i32 {
    if (n < 0) { return -1; }
    elif (n == 0) { return 0; }
    else { return 1; }
}

struct Point {
    x: i32;
    y: i32;

    func norm2(self) -> i32 {
        return self.x + self.y;
    }

    static func origin() -> Point {
        return Point { x: 0, y: 0 };
    }
}

func fallback(counter: i32*, value: i32*) -> i32* {
    unsafe { *counter += 1; }
    return value;
}

func shortcircuit() -> i32 {
    let counter: i32 = 0;
    let value: i32 = 7;
    unsafe {
        let present: i32* = &value;
        let absent: i32* = null;
        let a: i32* = present ?? fallback(&counter, present);
        let b: i32* = absent ?? fallback(&counter, present);
        return counter * 10 + *a + *b;
    }
}

func pairs() -> i32 {
    let pairs: (i32, i32)[] = [(1, 2), (3, 4)];
    let total: i32 = 0;
    for ((a, b) in pairs) { total += a + b; }
    return total;
}

func pattern(counter: i32*) -> i32 {
    unsafe { *counter += 1; return *counter; }
}

func matching() -> i32 {
    let counter: i32 = 0;
    unsafe {
        let result: i32 = match (1 as i32) {
            case pattern(&counter) => 10
            case pattern(&counter) => 20
            default => 30
        };
        return counter * 100 + result;
    }
}

func coverage() -> i32 {
    let values: i32[] = [1, 2, 3];
    let (a, (b, c)): (i32, (i32, i32)) = (1, (2, 3));
    let total: i32 = 0;
    for (value in values) { if (value == 2) { continue; } total += value; }
    for (i: i32 = 0; i < 3; i++) { if (i == 1) { continue; } total += i; }
    let pick: i32 = match total { case 6 => a case 9 => b default => c };
    let point: Point = Point { x: pick, y: 2 };
    total += point.norm2();
    total += add(2);
    let inc: func (v: i32) -> i32 = (v: i32): i32 => v + 1;
    let inc2: func (v: i32) -> i32 = (v: i32): i32 => v + 1;
    total += inc(1) + inc2(1);
    let index: i32 = 0;
    values[index++] += 5;
    unsafe {
        let empty: i32* = null;
        let backup: i32* = &total;
        let selected: i32* = empty ?? backup;
        total = *selected;
    }
    let pair: (i32, i32) = (a, b);
    with pair as (left, right) { total += left + right; }
    return total + values[0];
}

func sum(n: i32) -> i32 {
    let total: i32 = 0;
    let i: i32 = 0;
    while (i < n) { total += i; i = i + 1; }
    return total;
}"#;

/// 只认 `ENTRY` 的内存加载器。
struct MemoryLoader;

impl FileLoader for MemoryLoader {
    fn read_to_string(&mut self, path: &Path) -> io::Result<String> {
        if path.to_string_lossy() == ENTRY {
            Ok(SOURCE.to_string())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "virtual fs"))
        }
    }

    fn exists(&mut self, path: &Path) -> bool {
        path.to_string_lossy() == ENTRY
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut json = false;
    let mut path = None;
    for argument in std::env::args().skip(1) {
        if argument == "--json" {
            json = true;
        } else if path.replace(argument).is_some() {
            return Err("只接受一个输入文件".into());
        }
    }
    let result = match path {
        Some(path) => analyze(Path::new(&path)),
        None => analyze_with(&mut MemoryLoader, Path::new(ENTRY)),
    };
    for module in &result.modules.modules {
        for diagnostic in &module.syntax_diagnostics {
            eprintln!("{}: {}", module.path.display(), diagnostic.message);
        }
    }
    for diagnostic in &result.diagnostics.global {
        eprintln!("{}", diagnostic.message);
    }
    for module in &result.diagnostics.modules {
        for diagnostic in &module.diagnostics {
            eprintln!("{}: {}", module.path.display(), diagnostic.message);
        }
    }
    let hir = novella_hir::build_hir(&result)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&hir)?);
    } else {
        println!("{hir:#?}");
    }
    Ok(())
}
