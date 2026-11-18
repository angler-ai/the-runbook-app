//! CF12: the portable import fits one invocation's Workers Paid limits. Its
//! request makes one subrequest per D1 query, R2 operation and object
//! request, so these tests count each kind through the composed Cloudflare
//! store (natively: D1 on rusqlite, objects behind the object protocol, R2
//! in memory) and hold them to `import_cost`:
//! - the object requests and R2 operations grow by exactly the formula's
//!   coefficients (two per run, two per blob or source at worst);
//! - the D1 queries stay under the constant `IMPORT_D1_QUERIES`, whatever
//!   the archive holds;
//! - an archive whose cost does not fit the budget is refused before any
//!   write, and one that fits exactly imports.
//!
//! `cloudflare/tests/portable.test.mjs` checks the same boundary in workerd.
use runbook_core::{
    StoreError,
    directory::memory::MemoryDirectory,
    memory::MemoryExecutionStore,
    portable::{self, Archive, ImportOptions, PortableError, ProjectData},
    webhooks::memory::MemoryWebhooks,
};
use runbook_store_conformance::portable::{self as fixture, p, q};
use runbook_store_d1::{D1, D1Error, Statement, native::NativeD1, sql::Outcome};
use runbook_store_r2::{Listing, MemoryBucket, ObjectBucket};
use runbook_worker::{
    portable::{self as cf, IMPORT_D1_QUERIES, ImportBudget},
    protocol::{Reply, Request, Transport},
};
use runbook_worker_conformance::{
    cross::{CrossFactory, CrossHost, InProcess, NativeCross},
    native::run,
};
use std::{
    cell::Cell,
    future::Future,
    pin::pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

const T0: i64 = 1_700_000_000_000;
const CREATED: i64 = 1_800_000_000_000;
const STACK: usize = 32 * 1024 * 1024;

fn ready<T>(f: impl Future<Output = T>) -> T {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory adapters must be synchronous"),
    }
}

/// The shared fixture's project P, exported by the memory adapters.
fn source() -> Archive {
    source_with(0)
}
/// The fixture with `extra` more runs of its plain runbook in P.
fn source_with(extra: u8) -> Archive {
    let directory = MemoryDirectory::default();
    directory.set_time_ms(T0);
    let store = MemoryExecutionStore::empty(p(), T0);
    let webhooks = MemoryWebhooks::new(T0);
    ready(fixture::build(
        &store,
        &store.scoped(q()),
        &directory,
        &webhooks,
    ));
    for n in 0..extra {
        ready(fixture::start(&store, &directory, &p(), 100 + n, 2));
    }
    let data = portable::memory::export(&store, &directory, &webhooks).unwrap();
    portable::encode(&data, CREATED).unwrap()
}

fn on_big_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(STACK)
        .spawn(body)
        .unwrap()
        .join()
        .unwrap();
}

/// Calls by kind; each is one subrequest on Workers.
#[derive(Clone, Default)]
struct Counts {
    d1: Rc<Cell<u64>>,
    objects: Rc<Cell<u64>>,
    r2: Rc<Cell<u64>>,
    /// The most statements one D1 batch held.
    largest: Rc<Cell<usize>>,
    /// A fault: the next batch of at least this many statements loses its
    /// reply (D1 answers an error), after it commits (`true`) or before it
    /// runs (`false`).
    lose: Rc<Cell<Option<(usize, bool)>>>,
    /// CF12b: the same fault for the next batch with a statement whose SQL
    /// contains this text.
    lose_sql: Rc<std::cell::RefCell<Option<(&'static str, bool)>>>,
}
impl Counts {
    fn reset(&self) {
        self.d1.set(0);
        self.objects.set(0);
        self.r2.set(0);
        self.largest.set(0);
    }
    fn total(&self) -> u64 {
        self.d1.get() + self.objects.get() + self.r2.get()
    }
}
fn bump(cell: &Cell<u64>) {
    cell.set(cell.get() + 1);
}

#[derive(Clone)]
struct CountingD1(NativeD1, Counts);
impl D1 for CountingD1 {
    async fn all(&self, statement: Statement) -> Result<Vec<runbook_store_d1::sql::Row>, D1Error> {
        bump(&self.1.d1);
        self.0.all(statement).await
    }
    async fn run(&self, statement: Statement) -> Result<u64, D1Error> {
        bump(&self.1.d1);
        self.0.run(statement).await
    }
    async fn batch(&self, statements: Vec<Statement>) -> Result<Vec<Outcome>, D1Error> {
        bump(&self.1.d1);
        self.1
            .largest
            .set(self.1.largest.get().max(statements.len()));
        if let Some((at_least, landed)) = self.1.lose.get()
            && statements.len() >= at_least
        {
            self.1.lose.set(None);
            if landed {
                self.0.batch(statements).await?;
            }
            return Err(D1Error::Other("network connection lost".into()));
        }
        let marked = self
            .1
            .lose_sql
            .borrow()
            .filter(|(text, _)| statements.iter().any(|s| s.sql.contains(text)));
        if let Some((_, landed)) = marked {
            *self.1.lose_sql.borrow_mut() = None;
            if landed {
                self.0.batch(statements).await?;
            }
            return Err(D1Error::Other("network connection lost".into()));
        }
        self.0.batch(statements).await
    }
    fn now_ms(&self) -> i64 {
        self.0.now_ms()
    }
}

#[derive(Clone)]
struct CountingObjects(InProcess, Counts);
impl Transport for CountingObjects {
    async fn call(&self, request: Request) -> Reply {
        bump(&self.1.objects);
        self.0.call(request).await
    }
    async fn call_org(&self, request: runbook_worker::quota::QuotaRequest) -> Reply {
        bump(&self.1.objects);
        self.0.call_org(request).await
    }
    async fn call_webhook(&self, request: runbook_worker::webhooks::WebhookRequest) -> Reply {
        bump(&self.1.objects);
        self.0.call_webhook(request).await
    }
    fn clock(&self) -> Option<i64> {
        self.0.clock()
    }
}

#[derive(Clone)]
struct CountingBucket(MemoryBucket, Counts);
impl ObjectBucket for CountingBucket {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        bump(&self.1.r2);
        self.0.get(key).await
    }
    async fn get_range(
        &self,
        key: &str,
        offset: u64,
        length: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        bump(&self.1.r2);
        self.0.get_range(key, offset, length).await
    }
    async fn head(&self, key: &str) -> Result<Option<u64>, StoreError> {
        bump(&self.1.r2);
        self.0.head(key).await
    }
    async fn put_new(&self, key: &str, bytes: Vec<u8>) -> Result<bool, StoreError> {
        bump(&self.1.r2);
        self.0.put_new(key, bytes).await
    }
    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        bump(&self.1.r2);
        self.0.delete(key).await
    }
    async fn list(&self, prefix: &str, cursor: Option<String>) -> Result<Listing, StoreError> {
        bump(&self.1.r2);
        self.0.list(prefix, cursor).await
    }
}

/// The native cross host with every call counted.
#[derive(Default)]
struct Counting(Counts);
impl CrossHost for Counting {
    type Transport = CountingObjects;
    type D1 = CountingD1;
    type Bucket = CountingBucket;
    fn transport(&self, clock: Rc<Cell<i64>>) -> CountingObjects {
        CountingObjects(NativeCross.transport(clock), self.0.clone())
    }
    async fn d1(&self) -> Result<CountingD1, StoreError> {
        Ok(CountingD1(NativeCross.d1().await?, self.0.clone()))
    }
    async fn buckets(&self) -> Result<(CountingBucket, CountingBucket), StoreError> {
        let (blobs, content) = NativeCross.buckets().await?;
        Ok((
            CountingBucket(blobs, self.0.clone()),
            CountingBucket(content, self.0.clone()),
        ))
    }
    fn start_ms(&self) -> i64 {
        NativeCross.start_ms()
    }
}

/// The import's D1 plan of `data`.
async fn plan(db: &impl D1, data: &ProjectData) -> runbook_store_d1::portable::ImportPlan {
    runbook_store_d1::portable::import_statements(db, data, ImportOptions::default(), true)
        .await
        .unwrap()
}
/// The fixture's runbook sources the import stores in R2 (`plan.sources`).
async fn sources(db: &impl D1, data: &ProjectData) -> usize {
    plan(db, data).await.sources.len()
}

#[test]
fn a_committed_import_spends_what_its_cost_says() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let planned = plan(&factory.db().unwrap(), &data).await;
            let sources = planned.sources.len();
            let cost = cf::import_cost(&data, &planned);
            let counts = factory.host().0.clone();
            counts.reset();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let runs = data.rows.runs.len() as u64;
            let blobs = data.rows.blobs.len() as u64;
            assert!(runs >= 4 && blobs >= 2 && sources >= 1);
            // Each run: its install and its release.
            assert_eq!(counts.objects.get(), 2 * runs);
            // Each blob and source: one conditional put into an empty bucket.
            assert_eq!(counts.r2.get(), blobs + sources as u64);
            assert!(
                counts.d1.get() <= IMPORT_D1_QUERIES,
                "{} D1 queries",
                counts.d1.get()
            );
            assert!(counts.total() <= cost.subrequests);
            assert!(counts.largest.get() as u64 <= cost.statements);
            assert_eq!(cost.d1_queries, IMPORT_D1_QUERIES);
        });
    });
}

/// CF12b review round 1 (finding 3): an import reports progress as it goes,
/// which keeps its hold on the isolate's import gate fresh however long it
/// runs: at most the plan's three reads pass between two reports, and one
/// subrequest each after that, however many runs and blobs it holds.
#[test]
fn an_import_reports_progress_between_its_subrequests() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let counts = factory.host().0.clone();
            counts.reset();
            // The subrequests made so far, at each report.
            let at = std::cell::RefCell::new(vec![0u64]);
            let progress = || at.borrow_mut().push(counts.total());
            cf::import_watched(
                &store,
                &data,
                ImportOptions::default(),
                T0,
                ImportBudget::PAID,
                &progress,
            )
            .await
            .unwrap();
            let mut at = at.into_inner();
            at.push(counts.total());
            let gaps: Vec<u64> = at.windows(2).map(|w| w[1] - w[0]).collect();
            assert!(counts.total() > 10, "{}", counts.total());
            assert!(gaps.iter().take(3).all(|g| *g <= 3), "{gaps:?}");
            assert!(gaps.iter().skip(3).all(|g| *g <= 1), "{gaps:?}");
        });
    });
}

/// The D1 queries an import makes do not grow with its runs (the release
/// rows go in one batch), and its object requests grow by two a run.
#[test]
fn the_d1_queries_do_not_grow_with_the_runs() {
    on_big_stack(|| {
        let spent = |archive: Archive| {
            let (_, data) = portable::decode(&archive).unwrap();
            run(async move {
                let factory = CrossFactory::new(Counting::default());
                let (store, _) = factory.language(p(), T0).await.unwrap();
                let counts = factory.host().0.clone();
                counts.reset();
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .unwrap();
                (
                    data.rows.runs.len() as u64,
                    counts.d1.get(),
                    counts.objects.get(),
                )
            })
        };
        let (small_runs, small_d1, small_objects) = spent(source());
        let (big_runs, big_d1, big_objects) = spent(source_with(20));
        assert_eq!(big_runs, small_runs + 20);
        assert_eq!(big_d1, small_d1, "D1 queries by run count");
        assert_eq!(big_objects, small_objects + 2 * 20);
    });
}

/// Planning reads the directory's known users in one batch, however many
/// the archive names (200 more here than the fixture's).
#[test]
fn the_planning_reads_do_not_grow_with_the_directory() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let _store = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let counts = factory.host().0.clone();
            let mut more = data.clone();
            let template = more.directory.users[0].clone();
            for n in 0..200u32 {
                let mut user = template.clone();
                user.id = format!("usr_{:026}", 1_000 + n).parse().unwrap();
                user.name = format!("user {n}");
                more.directory.users.push(user);
            }
            counts.reset();
            sources(&db, &data).await;
            let few = counts.d1.get();
            counts.reset();
            sources(&db, &more).await;
            assert_eq!(counts.d1.get(), few, "D1 queries by user count");
        });
    });
}

/// CF11's split: the organization on its own shard, so the guards and the
/// commit are two batches each, and a failed control batch takes the
/// shard's rows back. Still under the constant, committed or not.
#[test]
fn a_sharded_import_stays_under_the_d1_constant() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let counting = Counting::default();
            let counts = counting.0.clone();
            let clock = Rc::new(Cell::new(T0));
            let sharded = || async {
                let control = counting.d1().await.unwrap();
                let shard = counting.d1().await.unwrap();
                runbook_worker_conformance::host::fresh_d1(&control)
                    .await
                    .unwrap();
                runbook_worker_conformance::host::fresh_d1(&shard)
                    .await
                    .unwrap();
                let map = runbook_worker::shards::ShardMap::parse(Some(&format!(
                    r#"{{"control": "DB", "default": "DB", "orgs": {{"{}": "DB_2"}}}}"#,
                    p().org_id
                )))
                .unwrap();
                let shards = runbook_worker::shards::D1Shards::open(map, |name| {
                    Ok::<_, ()>(if name == "DB_2" {
                        shard.clone()
                    } else {
                        control.clone()
                    })
                })
                .unwrap();
                let (blobs, content) = counting.buckets().await.unwrap();
                let store = runbook_worker::compose::CfStore::sharded(
                    shards,
                    runbook_worker::protocol::RemoteObjects::tracked(
                        counting.transport(clock.clone()),
                    ),
                    p(),
                    blobs,
                    content,
                );
                assert!(!store.colocated());
                (store, control)
            };
            // Committed: guards and commit on both databases.
            let (store, _) = sharded().await;
            counts.reset();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            assert!(counts.d1.get() <= IMPORT_D1_QUERIES);
            // The control batch fails after the shard's committed: a run's
            // release row is already another project's.
            let (store, control) = sharded().await;
            let id = &data.rows.runs[0].slice.run.projection.execution_id;
            control
                .run(
                    runbook_store_d1::prepare(
                        "INSERT INTO run_release (execution_id, org_id, project_id, \
                         created_at_ms) VALUES (?, 'org_other', 'prj_other', 0)",
                    )
                    .bind(id.as_str()),
                )
                .await
                .unwrap();
            counts.reset();
            assert!(
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .is_err()
            );
            // A refused batch (a constraint) needs no read-back: two short of
            // the worst case, a lost reply on each commit batch.
            assert_eq!(counts.d1.get(), IMPORT_D1_QUERIES - 2);
        });
    });
}

#[test]
fn a_failed_commit_spends_no_more_than_its_cost() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let sources = sources(&db, &data).await;
            // A webhook subscription ID already used elsewhere: the guards
            // pass, everything is written, the D1 batch fails, and the
            // import undoes it all (the most an import ever spends).
            let webhook = data.webhooks.first().expect("the fixture has a webhook");
            db.run(
                runbook_store_d1::prepare(
                    "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                     created_at_ms, body) VALUES (?, 'org_other', 'prj_other', 0, '{}')",
                )
                .bind(webhook.id.as_str()),
            )
            .await
            .unwrap();
            let counts = factory.host().0.clone();
            counts.reset();
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Conflict("import.id_conflict"))
            );
            let runs = data.rows.runs.len() as u64;
            let items = runs + data.rows.blobs.len() as u64 + sources as u64;
            // Each run: its install and its purge.
            assert_eq!(counts.objects.get(), 2 * runs);
            // Each blob and source: its put and its delete.
            assert_eq!(counts.r2.get(), 2 * (items - runs));
            assert!(
                counts.d1.get() <= IMPORT_D1_QUERIES,
                "{} D1 queries",
                counts.d1.get()
            );
            assert!(counts.total() <= cf::import_cost(&data, &plan(&db, &data).await).subrequests);
        });
    });
}

#[test]
fn an_import_past_its_budget_writes_nothing_and_one_at_it_imports() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let cost = cf::import_cost(&data, &plan(&factory.db().unwrap(), &data).await);
            let exactly = ImportBudget {
                subrequests: cost.subrequests,
                d1_queries: cost.d1_queries,
                batch_statements: cost.statements,
            };
            let counts = factory.host().0.clone();
            for short in [
                ImportBudget {
                    subrequests: cost.subrequests - 1,
                    ..exactly
                },
                ImportBudget {
                    d1_queries: cost.d1_queries - 1,
                    ..exactly
                },
                ImportBudget {
                    batch_statements: cost.statements - 1,
                    ..exactly
                },
            ] {
                counts.reset();
                assert_eq!(
                    cf::import_within(&store, &data, ImportOptions::default(), T0, short).await,
                    Err(PortableError::Archive {
                        code: cf::TOO_LARGE,
                        path: None
                    })
                );
                assert_eq!(counts.objects.get(), 0, "no object was touched");
                assert_eq!(counts.r2.get(), 0, "no blob was written");
            }
            assert_eq!(
                cf::import_within(&store, &data, ImportOptions::default(), T0, exactly).await,
                Ok(cost)
            );
            let again = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            assert_eq!(again, source());
        });
    });
}

/// The Paid budget, in archive terms: two subrequests per run, blob or
/// source after the D1 queries, under 10,000 less the route's reserve.
#[test]
fn the_paid_budget_admits_this_many_items() {
    assert_eq!(ImportBudget::PAID.subrequests, 9_900);
    assert_eq!(ImportBudget::PAID.d1_queries, 900);
    let items = (ImportBudget::PAID.subrequests - IMPORT_D1_QUERIES) / 2;
    assert_eq!(items, cf::MAX_IMPORT_ITEMS);
}

/// CF12 review round 1: the export's D1 queries are a constant and each run
/// is one object request, so an export's worst case is known from the R2
/// listing and the run count before any run is read.
#[test]
fn an_export_spends_what_its_cost_says() {
    on_big_stack(|| {
        let spent = |archive: Archive| {
            let (_, data) = portable::decode(&archive).unwrap();
            run(async move {
                let factory = CrossFactory::new(Counting::default());
                let (store, _) = factory.language(p(), T0).await.unwrap();
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .unwrap();
                let counts = factory.host().0.clone();
                counts.reset();
                let (exported, cost) = cf::export_within(&store, ImportBudget::PAID).await.unwrap();
                assert_eq!(portable::encode(&exported, CREATED).unwrap(), archive);
                let runs = data.rows.runs.len() as u64;
                assert_eq!(counts.objects.get(), runs, "one object request a run");
                assert!(
                    counts.d1.get() <= cf::EXPORT_D1_QUERIES,
                    "{} D1 queries",
                    counts.d1.get()
                );
                assert!(counts.total() <= cost.subrequests);
                assert_eq!(cost.d1_queries, cf::EXPORT_D1_QUERIES);
                (runs, counts.d1.get())
            })
        };
        let (small_runs, small_d1) = spent(source());
        let (big_runs, big_d1) = spent(source_with(20));
        assert_eq!(big_runs, small_runs + 20);
        assert_eq!(big_d1, small_d1, "D1 queries by run count");
    });
}

#[test]
fn an_export_past_its_budget_reads_no_run() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let (_, cost) = cf::export_within(&store, ImportBudget::PAID).await.unwrap();
            let counts = factory.host().0.clone();
            counts.reset();
            let short = ImportBudget {
                subrequests: cost.subrequests - 1,
                ..ImportBudget::PAID
            };
            assert_eq!(
                cf::export_within(&store, short).await.map(|(_, c)| c),
                Err(PortableError::Archive {
                    code: cf::EXPORT_TOO_LARGE,
                    path: None
                })
            );
            assert_eq!(counts.objects.get(), 0, "no run was read");
        });
    });
}

/// `data` with blobs added to its first run until its import's worst case
/// is the Paid budget (or one under it, the cost being odd).
fn at_the_item_cap(mut data: ProjectData, sources: usize) -> ProjectData {
    let room =
        cf::MAX_IMPORT_ITEMS - (data.rows.runs.len() + data.rows.blobs.len() + sources) as u64;
    let run = &mut data.rows.runs[0];
    let id = run.slice.run.projection.execution_id.clone();
    for n in 0..room {
        let key = runbook_core::blob::BlobKey::Execution {
            execution_id: id.clone(),
            name: format!("cap{n:05}"),
        };
        run.slice.blob_keys.push(key.clone());
        data.rows.blobs.push(portable::BlobRow {
            key,
            bytes: n.to_le_bytes().to_vec(),
        });
    }
    portable::normalize(&mut data);
    data
}

/// Anything the import admits, the export emits under the same budget: an
/// archive at the import's item cap imports under the Paid budget, then
/// exports under it, unchanged.
#[test]
fn an_export_after_an_import_at_the_item_cap_fits_the_paid_budget() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let sources = sources(&factory.db().unwrap(), &data).await;
            let data = at_the_item_cap(data, sources);
            let counts = factory.host().0.clone();
            counts.reset();
            let cost = cf::import_within(
                &store,
                &data,
                ImportOptions::default(),
                T0,
                ImportBudget::PAID,
            )
            .await
            .unwrap();
            assert!(
                cost.subrequests + 1 >= ImportBudget::PAID.subrequests,
                "{cost:?}"
            );
            assert!(counts.total() <= ImportBudget::PAID.subrequests);
            counts.reset();
            let (exported, cost) = cf::export_within(&store, ImportBudget::PAID).await.unwrap();
            assert!(cost.subrequests <= ImportBudget::PAID.subrequests);
            assert!(counts.total() <= cost.subrequests, "{}", counts.total());
            assert!(counts.d1.get() <= ImportBudget::PAID.d1_queries);
            assert_eq!(
                portable::encode(&exported, CREATED).unwrap(),
                portable::encode(&data, CREATED).unwrap()
            );
        });
    });
}

/// `data` with `n` more tombstones (each a guard and a row in the commit
/// batch, and no subrequest).
fn with_tombstones(mut data: ProjectData, n: usize) -> ProjectData {
    let template = runbook_core::retention::Tombstone {
        scope: data.scope.clone(),
        execution_id: data.rows.runs[0].slice.run.projection.execution_id.clone(),
        runbook_id: data.rows.runs[0].slice.run.projection.runbook_id.clone(),
        deleted_at_ms: T0,
        reason: "manual".into(),
    };
    for i in 0..n {
        let mut tombstone = template.clone();
        tombstone.execution_id = format!("exe_7{i:025}").parse().unwrap();
        data.rows.tombstones.push(tombstone);
    }
    portable::normalize(&mut data);
    data
}

/// CF12 review round 1: every D1 batch an import sends (its guards, its
/// commit, its undo) holds at most the budget's statements, so each fits
/// D1's 30-second limit on a whole batch. Tombstones cost no subrequest,
/// so before this an archive of many of them passed the budget with an
/// unbounded commit batch.
#[test]
fn the_commit_batch_is_held_to_the_statement_bound() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let data = with_tombstones(data, cf::MAX_BATCH_STATEMENTS as usize);
            let plan = plan(&db, &data).await;
            let cost = cf::import_cost(&data, &plan);
            assert!(cost.statements > cf::MAX_BATCH_STATEMENTS, "{cost:?}");
            let counts = factory.host().0.clone();
            counts.reset();
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Archive {
                    code: cf::TOO_LARGE,
                    path: None
                })
            );
            assert_eq!(counts.objects.get() + counts.r2.get(), 0, "nothing written");
            // At exactly its statement count it imports, and no batch it
            // sent held more.
            let exactly = ImportBudget {
                batch_statements: cost.statements,
                ..ImportBudget::PAID
            };
            let short = ImportBudget {
                batch_statements: cost.statements - 1,
                ..exactly
            };
            assert!(
                cf::import_within(&store, &data, ImportOptions::default(), T0, short)
                    .await
                    .is_err()
            );
            counts.reset();
            assert_eq!(
                cf::import_within(&store, &data, ImportOptions::default(), T0, exactly).await,
                Ok(cost)
            );
            assert!(counts.largest.get() as u64 <= cost.statements);
        });
    });
}

/// The guards and the undo grow by one statement per 90 runs or
/// tombstones, not one per run.
#[test]
fn the_guards_and_the_undo_are_chunked() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let _store = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let few = plan(&db, &data).await.guards().len();
            let many = with_tombstones(data.clone(), 900);
            assert_eq!(plan(&db, &many).await.guards().len(), few + 10);
            let ids: Vec<runbook_model::ExecutionId> = (0..900)
                .map(|i| format!("exe_7{i:025}").parse().unwrap())
                .collect();
            assert_eq!(
                runbook_store_d1::portable::forget_run_rows(&p(), &ids).len(),
                8 * 10
            );
        });
    });
}

/// CF12 review round 1: a commit whose reply is lost (D1 answers an error
/// after the batch committed) is read back before anything is undone: the
/// import stands, its runs are released, and it exports unchanged.
#[test]
fn a_commit_that_landed_is_read_back_not_undone() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let plan = plan(&db, &data).await;
            let counts = factory.host().0.clone();
            let commit = plan.guards().len() + plan.control.len() + plan.shard.len();
            counts.lose.set(Some((commit, true)));
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .map(|_| ()),
                Ok(())
            );
            assert_eq!(counts.lose.get(), None, "the fault fired");
            let again = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            assert_eq!(again, source());
        });
    });
}

/// A commit lost before it ran is read back too: nothing landed, so the
/// import is undone and a retry imports.
#[test]
fn a_commit_that_did_not_land_is_undone() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let plan = plan(&db, &data).await;
            let counts = factory.host().0.clone();
            let commit = plan.guards().len() + plan.control.len() + plan.shard.len();
            counts.lose.set(Some((commit, false)));
            assert!(
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .is_err()
            );
            assert_eq!(counts.lose.get(), None, "the fault fired");
            cf::import(&store, &data, ImportOptions::default(), T0)
                .await
                .unwrap();
            let again = portable::encode(&cf::export(&store).await.unwrap(), CREATED).unwrap();
            assert_eq!(again, source());
        });
    });
}

/// `data` with tombstones added until its import's largest D1 batch is
/// exactly `bound` statements.
async fn at_the_statement_bound(db: &impl D1, data: ProjectData, bound: u64) -> ProjectData {
    let statements = |d: &ProjectData| {
        let d = d.clone();
        async move { cf::import_cost(&d, &plan(db, &d).await).statements }
    };
    let base = statements(&data).await;
    assert!(base < bound, "{base}");
    // Each tombstone is one row, plus a guard per 90: start under, then
    // step up one at a time.
    let mut n = ((bound - base) * 90 / 91) as usize;
    loop {
        let candidate = with_tombstones(data.clone(), n);
        match statements(&candidate).await {
            s if s == bound => return candidate,
            s if s < bound => n += 1,
            s => panic!("{s} past {bound} at {n}"),
        }
    }
}

/// CF12 review round 1: an archive at both caps at once (the item cap in
/// blobs, the statement bound in 150 more runs and tombstones) fails its
/// commit at the last moment (the most an import spends) and undoes it all
/// with no batch past the bound; then, the conflict removed, it imports.
#[test]
fn an_import_at_both_caps_fails_and_commits_within_them() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source_with(150)).unwrap();
        run(async move {
            let factory = CrossFactory::new(Counting::default());
            let (store, _) = factory.language(p(), T0).await.unwrap();
            let db = factory.db().unwrap();
            let data = at_the_statement_bound(&db, data, cf::MAX_BATCH_STATEMENTS).await;
            let sources = plan(&db, &data).await.sources.len();
            let data = at_the_item_cap(data, sources);
            let cost = cf::import_cost(&data, &plan(&db, &data).await);
            assert_eq!(cost.statements, cf::MAX_BATCH_STATEMENTS);
            assert!(
                cost.subrequests + 1 >= ImportBudget::PAID.subrequests,
                "{cost:?}"
            );
            // A webhook ID another project holds: everything is written,
            // then the commit fails.
            let webhook = data.webhooks.first().expect("the fixture has a webhook");
            let taken =
                |sql: &'static str| runbook_store_d1::prepare(sql).bind(webhook.id.as_str());
            db.run(taken(
                "INSERT INTO webhook_subscription (subscription_id, org_id, project_id, \
                 created_at_ms, body) VALUES (?, 'org_other', 'prj_other', 0, '{}')",
            ))
            .await
            .unwrap();
            let counts = factory.host().0.clone();
            counts.reset();
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0).await,
                Err(PortableError::Conflict("import.id_conflict"))
            );
            assert!(counts.largest.get() as u64 <= cf::MAX_BATCH_STATEMENTS);
            assert!(counts.total() <= cost.subrequests, "{}", counts.total());
            assert!(counts.d1.get() <= IMPORT_D1_QUERIES);
            db.run(taken(
                "DELETE FROM webhook_subscription WHERE subscription_id = ?",
            ))
            .await
            .unwrap();
            counts.reset();
            assert_eq!(
                cf::import(&store, &data, ImportOptions::default(), T0)
                    .await
                    .map(|_| ()),
                Ok(())
            );
            assert_eq!(counts.largest.get() as u64, cf::MAX_BATCH_STATEMENTS);
            assert!(counts.total() <= cost.subrequests);
        });
    });
}

/// `data` with only its catalog: no runs, tombstones, variables, memory,
/// webhooks or blobs other than the shared ones its versions need.
fn catalog_only(mut data: ProjectData) -> ProjectData {
    data.rows.runs.clear();
    data.rows.tombstones.clear();
    data.rows.memory.clear();
    data.rows.pools.clear();
    data.rows
        .blobs
        .retain(|b| matches!(b.key, runbook_core::blob::BlobKey::Shared { .. }));
    data.variables = None;
    data.webhooks.clear();
    data
}

/// CF12b (finding 4): an import whose organization has its own shard, with
/// no runs and no variables, so that its control batch writes only the
/// directory. That batch lands and its reply is lost: the read-back sees
/// the directory's record of the project and the import stands, where it
/// used to take back the shard's rows and leave a directory a retry could
/// only answer with `import.org_exists`.
#[test]
fn a_directory_only_control_batch_that_landed_is_read_back() {
    on_big_stack(|| {
        let (_, data) = portable::decode(&source()).unwrap();
        let data = catalog_only(data);
        assert!(data.rows.runs.is_empty() && data.variables.is_none());
        run(async move {
            let counting = Counting::default();
            let counts = counting.0.clone();
            let clock = Rc::new(Cell::new(T0));
            let control = counting.d1().await.unwrap();
            let shard = counting.d1().await.unwrap();
            runbook_worker_conformance::host::fresh_d1(&control)
                .await
                .unwrap();
            runbook_worker_conformance::host::fresh_d1(&shard)
                .await
                .unwrap();
            let map = runbook_worker::shards::ShardMap::parse(Some(&format!(
                r#"{{"control": "DB", "default": "DB", "orgs": {{"{}": "DB_2"}}}}"#,
                p().org_id
            )))
            .unwrap();
            let shards = runbook_worker::shards::D1Shards::open(map, |name| {
                Ok::<_, ()>(if name == "DB_2" {
                    shard.clone()
                } else {
                    control.clone()
                })
            })
            .unwrap();
            let (blobs, content) = counting.buckets().await.unwrap();
            let store = runbook_worker::compose::CfStore::sharded(
                shards,
                runbook_worker::protocol::RemoteObjects::tracked(counting.transport(clock.clone())),
                p(),
                blobs,
                content,
            );
            assert!(!store.colocated());
            // Only the control commit batch creates the organization.
            *counts.lose_sql.borrow_mut() = Some(("INSERT INTO organization", true));
            let imported = cf::import(&store, &data, ImportOptions::default(), T0).await;
            assert!(counts.lose_sql.borrow().is_none(), "the fault fired");
            assert_eq!(imported.map(|_| ()), Ok(()));
            let mut expected = data.clone();
            portable::normalize(&mut expected);
            let exported = cf::export(&store).await.unwrap();
            assert_eq!(
                portable::encode(&exported, CREATED).unwrap(),
                portable::encode(&expected, CREATED).unwrap()
            );
        });
    });
}
