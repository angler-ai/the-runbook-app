use runbook_expr::{
    EventTime, FieldDecl, InputNode, InputValue, Origin, RecordIdentity, SchemaDef, TypeDef,
    TypeEnv, TypeId,
};
use std::collections::BTreeMap;

pub fn time() -> EventTime {
    EventTime {
        unix_seconds: 0,
        nanos: 0,
    }
}
pub fn origin(path: impl Into<String>) -> Origin {
    Origin {
        path: path.into(),
        record: RecordIdentity {
            execution_id: "e".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision: 1,
        },
    }
}
pub fn node(path: impl Into<String>, value: InputValue) -> InputNode {
    InputNode {
        value,
        origin: origin(path),
    }
}
pub fn decl(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        required: true,
        nullable: false,
    }
}
pub fn env(types: Vec<TypeDef>, root: u32) -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types,
        roots: BTreeMap::from([("x".into(), decl(root))]),
    })
    .unwrap()
}
pub fn roots(value: InputValue) -> BTreeMap<String, InputNode> {
    BTreeMap::from([("x".into(), node("x", value))])
}

// The fixture remains caller owned even after constructor rejection.
pub fn dismantle(roots: BTreeMap<String, InputNode>) {
    let mut pending: Vec<_> = roots.into_values().collect();
    while let Some(node) = pending.pop() {
        match node.value {
            InputValue::List(items) => pending.extend(items),
            InputValue::Record(fields) => pending.extend(fields.into_values()),
            _ => {}
        }
    }
}

// Track only one caller-owned fixture allocation. This measures early-rejection
// destruction without changing InputNode or accepting caller callbacks.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
struct FixtureAllocator;
#[global_allocator]
static ALLOCATOR: FixtureAllocator = FixtureAllocator;
static WATCHED: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for FixtureAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forward the allocation request unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCHED
            .compare_exchange(ptr as usize, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
        // SAFETY: Forward the original system allocation and layout unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
}
pub fn watch_fixture_allocation(ptr: *const u8) {
    DROPS.store(0, Ordering::SeqCst);
    WATCHED.store(ptr as usize, Ordering::SeqCst);
}
pub fn fixture_drops() -> usize {
    DROPS.load(Ordering::SeqCst)
}
