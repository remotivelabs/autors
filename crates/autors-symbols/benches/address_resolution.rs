use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use autors_a2l::Project;
use autors_symbols::update::{address_node, modules_with_index, AddressNodeResolver};

const LAYOUT_COUNT: usize = 64;
const NODE_COUNT: usize = 4_000;

fn project() -> Project {
    let mut source = String::with_capacity(NODE_COUNT * 100);
    source.push_str("/begin PROJECT P \"benchmark\"\n/begin MODULE M \"module\"\n");
    for layout in 0..LAYOUT_COUNT {
        writeln!(
            source,
            "/begin RECORD_LAYOUT RL{layout} FNC_VALUES 1 ULONG ROW_DIR DIRECT /end RECORD_LAYOUT"
        )
        .expect("writing to a String cannot fail");
    }
    for node in 0..NODE_COUNT {
        writeln!(
            source,
            "/begin CHARACTERISTIC C{node} \"value\" VALUE 0x0 RL{} 0 NO_COMPU_METHOD 0 1 /end CHARACTERISTIC",
            node % LAYOUT_COUNT
        )
        .expect("writing to a String cannot fail");
    }
    source.push_str("/end MODULE\n/end PROJECT\n");
    Project::parse_str(&source).expect("synthetic A2L should parse")
}

fn resolve_linear(project: &Project) -> usize {
    modules_with_index(project)
        .flat_map(|(_, module)| &module.children)
        .filter_map(|child| address_node(project, child).expect("node resolution should succeed"))
        .filter(|node| node.record_layout.is_some())
        .count()
}

fn resolve_indexed(project: &Project) -> usize {
    let resolver = AddressNodeResolver::new(project);
    modules_with_index(project)
        .flat_map(|(_, module)| &module.children)
        .filter_map(|child| {
            resolver
                .address_node(child)
                .expect("node resolution should succeed")
        })
        .filter(|node| node.record_layout.is_some())
        .count()
}

fn best_of(mut operation: impl FnMut() -> usize, samples: usize) -> (Duration, usize) {
    let mut best = Duration::MAX;
    let mut count = 0;
    for _ in 0..samples {
        let start = Instant::now();
        count = black_box(operation());
        best = best.min(start.elapsed());
    }
    (best, count)
}

fn main() {
    let project = project();
    let (linear, linear_count) = best_of(|| resolve_linear(black_box(&project)), 3);
    let (indexed, indexed_count) = best_of(|| resolve_indexed(black_box(&project)), 10);
    assert_eq!(linear_count, NODE_COUNT);
    assert_eq!(indexed_count, NODE_COUNT);

    println!("address resolution ({NODE_COUNT} nodes, {LAYOUT_COUNT} layouts)");
    println!("  full-project scan: {linear:?}");
    println!("  indexed lookup:    {indexed:?}");
    println!(
        "  speedup:           {:.1}x",
        linear.as_secs_f64() / indexed.as_secs_f64()
    );
}
