//! Explicit, resumable premise-index embeddings. No source or reference SIR is an input.

use mls_core::{ContentHasher, source_hash};
use mls_engine::{
    UsageSummary,
    retrieval::{EmbeddingConfig, IndexManifest, PremiseIndex, PremiseVectors},
};
use mls_models::{
    CallContext, EmbeddingClient, EmbeddingInputType, EmbeddingReply, EmbeddingRequest,
    ModelConfig, ModelError, OpenRouterClient, RecordingClient, ReplayClient, Usage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    error::Error,
    ffi::OsString,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const INPUT_BUDGET: usize = 256 * 1024;
const STATE: &str = ".mls/retrieval-index-v1";

fn fail(message: impl Into<String>) -> Box<dyn Error> {
    std::io::Error::other(message.into()).into()
}

/// Dispatch the explicit retrieval-index command.
pub(crate) fn run(arguments: &[OsString]) -> Result<()> {
    let mut directory = None;
    let mut replay = None;
    let mut live = false;
    let mut batch_size = 64;
    let mut at = 0;
    while at < arguments.len() {
        let argument = arguments[at].to_string_lossy();
        match argument.as_ref() {
            "--live" if !live => live = true,
            "--replay" | "--batch-size" => {
                at += 1;
                let value = arguments
                    .get(at)
                    .ok_or_else(|| fail(format!("{argument} requires a value")))?;
                if argument == "--replay" {
                    if replay.replace(PathBuf::from(value)).is_some() {
                        return Err(fail("duplicate --replay"));
                    }
                } else {
                    batch_size = value
                        .to_string_lossy()
                        .parse::<usize>()
                        .map_err(|_| fail("--batch-size must be positive"))?;
                }
            }
            "--help" | "-h" => {
                println!(
                    "Usage: mls retrieval-index INDEX_DIR (--live | --replay RECORDINGS_DIR) [--batch-size N]"
                );
                return Ok(());
            }
            _ if argument.starts_with('-') => {
                return Err(fail(format!("unknown option {argument}")));
            }
            _ => {
                if directory.replace(PathBuf::from(&arguments[at])).is_some() {
                    return Err(fail("expected exactly one INDEX_DIR"));
                }
            }
        }
        at += 1;
    }
    if live == replay.is_some() {
        return Err(fail(
            "choose exactly one of --live or --replay RECORDINGS_DIR",
        ));
    }
    if batch_size == 0 {
        return Err(fail("--batch-size must be positive"));
    }
    let directory = directory
        .ok_or_else(|| fail("expected INDEX_DIR containing manifest.json and premises.jsonl"))?;
    if live && std::env::var("MLS_LIVE").as_deref() != Ok("1") {
        return Err(fail("--live requires MLS_LIVE=1"));
    }
    let config = ModelConfig::from_path(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/models.json"),
    )?;
    let report = if live {
        // Construct before creating state, so a missing credential never leaves a partial build.
        let mut client = OpenRouterClient::from_env(config.clone())?;
        build_with_client(&directory, &config, batch_size, &mut client)?
    } else {
        build_replay(
            &directory,
            &config,
            batch_size,
            &replay.expect("transport checked"),
        )?
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u32,
    manifest: IndexManifest,
    embedding: EmbeddingConfig,
    input_type: EmbeddingInputType,
    batch_size: usize,
    input_budget: usize,
    row_text_sha256: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    start: usize,
    end: usize,
    request_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    request_sha256: String,
    recording_sha256: String,
    usage: Option<Usage>,
    shard_sha256: Option<String>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u32,
    identity_sha256: String,
    batches: BTreeMap<String, Checkpoint>,
    content_sha256: String,
}

impl Ledger {
    fn checksum(&self) -> Result<String> {
        Ok(source_hash(&serde_json::to_vec(&(
            self.version,
            &self.identity_sha256,
            &self.batches,
        ))?))
    }
}

/// Honest accounting for all recorded attempts, including work recovered from earlier runs.
#[derive(Serialize)]
pub(crate) struct BuildReport {
    complete: bool,
    eligible_rows: usize,
    completed_batches: usize,
    new_calls: usize,
    recovered_batches: usize,
    usage_scope: &'static str,
    provider_usage: UsageSummary,
    vector_cache: PathBuf,
    recordings: PathBuf,
}

struct Build {
    index: PremiseIndex,
    identity: Identity,
    hash: String,
    plan: Vec<Batch>,
    ledger: Ledger,
    root: PathBuf,
    _lock: Lock,
    new_calls: usize,
    recovered: usize,
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl Build {
    fn prepare(directory: &Path, config: &ModelConfig, batch_size: usize) -> Result<Self> {
        if batch_size == 0 {
            return Err(fail("batch size must be positive"));
        }
        let directory = directory.canonicalize()?;
        plain_file(&directory.join("manifest.json"))?;
        plain_file(&directory.join("premises.jsonl"))?;
        let index = PremiseIndex::parse(
            &fs::read(directory.join("manifest.json"))?,
            &fs::read(directory.join("premises.jsonl"))?,
        )?;
        let identity = Identity {
            version: 1,
            manifest: index.manifest().clone(),
            embedding: EmbeddingConfig {
                model: config.retrieval.embedding_model.clone(),
                dimensions: config.retrieval.embedding_dimensions,
            },
            input_type: EmbeddingInputType::SearchDocument,
            batch_size,
            input_budget: INPUT_BUDGET,
            row_text_sha256: index
                .rows()
                .iter()
                .map(|row| source_hash(row.embedding_text().as_bytes()))
                .collect(),
        };
        if identity.embedding.model.trim().is_empty() || identity.embedding.dimensions == 0 {
            return Err(fail("invalid embedding configuration"));
        }
        let hash = source_hash(&serde_json::to_vec(&identity)?);
        let plan = make_plan(&index, &identity, &hash)?;
        let root = directory.join(STATE);
        plain_dir(&directory.join(".mls"))?;
        plain_dir(&root)?;
        let lock_path = root.join("build.lock");
        let mut lock = fs::OpenOptions::new().write(true).create_new(true).open(&lock_path)
            .map_err(|error| fail(format!("cannot acquire build lock {}: {error}; if interrupted, inspect its owner and remove this lock before resuming", lock_path.display())))?;
        let guard = Lock(lock_path);
        writeln!(lock, "pid={} identity={hash}", std::process::id())?;
        lock.sync_all()?;
        immutable_json(&root.join("identity.json"), &identity)?;
        immutable_json(&root.join("plan.json"), &plan)?;
        plain_dir(&root.join("shards"))?;
        plain_dir(&root.join("recordings"))?;
        let ledger_path = root.join("ledger.json");
        let ledger = if exists(&ledger_path)? {
            plain_file(&ledger_path)?;
            let ledger: Ledger = serde_json::from_slice(&fs::read(&ledger_path)?)?;
            if ledger.version != 1
                || ledger.identity_sha256 != hash
                || ledger.content_sha256 != ledger.checksum()?
            {
                return Err(fail("incompatible retrieval build ledger"));
            }
            ledger
        } else {
            Ledger {
                version: 1,
                identity_sha256: hash.clone(),
                batches: BTreeMap::new(),
                content_sha256: String::new(),
            }
        };
        for name in ledger.batches.keys() {
            if !plan.iter().enumerate().any(|(n, _)| batch_name(n) == *name) {
                return Err(fail("ledger contains an unknown batch"));
            }
        }
        // Validate every existing output and directory before a possible first live call.
        probe(&root)?;
        probe(&root.join("shards"))?;
        for (number, batch) in plan.iter().enumerate() {
            let name = batch_name(number);
            let batch_dir = root.join("recordings").join(&name);
            plain_dir(&batch_dir)?;
            let calls = batch_dir.join("attempt-0000");
            plain_dir(&calls)?;
            probe(&calls)?;
            let recording = only_record(&calls)?;
            if let Some(checkpoint) = ledger.batches.get(&name) {
                if checkpoint.request_sha256 != batch.request_sha256 {
                    return Err(fail("ledger batch request identity mismatch"));
                }
                let record = recording
                    .clone()
                    .ok_or_else(|| fail("ledger recording is missing"))?;
                if hash_file(&record)? != checkpoint.recording_sha256 {
                    return Err(fail("recording checksum mismatch"));
                }
            }
            let shard = root.join("shards").join(format!("{name}.f32"));
            if exists(&shard)? {
                if recording.is_none() {
                    return Err(fail("existing shard has no durable call recording"));
                }
                plain_file(&shard)?;
                let digest = validate_shard(
                    &shard,
                    batch.end - batch.start,
                    identity.embedding.dimensions,
                )?;
                if let Some(expected) = ledger
                    .batches
                    .get(&name)
                    .and_then(|c| c.shard_sha256.as_ref())
                    && *expected != digest
                {
                    return Err(fail("shard checksum mismatch"));
                }
            }
            let pending = calls.join("pending");
            if exists(&pending)? {
                plain_file(&pending)?;
                immutable_json(
                    &pending,
                    &serde_json::json!({"request_sha256":batch.request_sha256}),
                )?;
            }
        }
        let final_path = root.join("vectors.f32");
        if exists(&final_path)? {
            plain_file(&final_path)?;
            PremiseVectors::validate_stream(
                &index,
                identity.embedding.clone(),
                &mut File::open(&final_path)?,
            )?;
        }
        Ok(Self {
            index,
            identity,
            hash,
            plan,
            ledger,
            root,
            _lock: guard,
            new_calls: 0,
            recovered: 0,
        })
    }

    fn execute(
        &mut self,
        config: &ModelConfig,
        network: bool,
        mut submit: impl FnMut(&EmbeddingRequest, &Path) -> Result<()>,
    ) -> Result<BuildReport> {
        self.preflight_recordings(config, network)?;
        for number in 0..self.plan.len() {
            let batch = self.plan[number].clone();
            let name = batch_name(number);
            let calls = self
                .root
                .join("recordings")
                .join(&name)
                .join("attempt-0000");
            let shard = self.root.join("shards").join(format!("{name}.f32"));
            if let Some(checkpoint) = self.ledger.batches.get(&name) {
                if checkpoint.error.is_some() {
                    return Err(fail(format!(
                        "batch {name} has a recorded failure; no automatic network retry: inspect {}",
                        calls.display()
                    )));
                }
                if exists(&shard)? && checkpoint.shard_sha256.is_some() {
                    continue;
                }
            }
            let request = request(&self.index, &self.identity, &self.hash, number, &batch);
            let previously_recorded = only_record(&calls)?.is_some();
            if !previously_recorded {
                if network && exists(&calls.join("pending"))? {
                    return Err(fail(format!(
                        "batch {name} was interrupted without a durable outcome; it may have been billed; no automatic retry: inspect {}",
                        calls.display()
                    )));
                }
                immutable_json(
                    &calls.join("pending"),
                    &serde_json::json!({"request_sha256":batch.request_sha256}),
                )?;
                if network {
                    self.new_calls += 1;
                }
                if let Err(error) = submit(&request, &calls)
                    && only_record(&calls)?.is_none()
                {
                    return Err(error);
                }
            }
            // Replay is the source of truth even for a just-returned result: do not checkpoint
            // vectors whose exact call record failed to persist.
            let record = only_record(&calls)?
                .ok_or_else(|| fail("embedding attempt has no persisted recording"))?;
            sync_parent(&record)?;
            let mut replay = ReplayClient::from_directory(
                &calls,
                &config.generation_model,
                &config.decision_model,
            )?;
            let outcome = replay.embed(&request);
            let mut checkpoint = Checkpoint {
                request_sha256: batch.request_sha256,
                recording_sha256: hash_file(&record)?,
                usage: normalized_usage(&outcome),
                shard_sha256: None,
                error: None,
            };
            // Account durable evidence before any later payload/filesystem validation can fail.
            self.ledger.batches.insert(name.clone(), checkpoint.clone());
            self.save_ledger()?;
            match outcome {
                Ok(reply) => {
                    match payload(
                        &reply,
                        self.identity.embedding.dimensions,
                        batch.end - batch.start,
                    ) {
                        Ok(bytes) => {
                            let digest = source_hash(&bytes);
                            if exists(&shard)? {
                                if hash_file(&shard)? != digest {
                                    return Err(fail(
                                        "existing shard differs from its recorded vectors",
                                    ));
                                }
                            } else {
                                create_bytes(&shard, &bytes)?;
                            }
                            checkpoint.shard_sha256 = Some(digest);
                        }
                        Err(error) => checkpoint.error = Some(error.to_string()),
                    }
                }
                Err(error) => checkpoint.error = Some(error.to_string()),
            }
            let error = checkpoint.error.clone();
            self.ledger.batches.insert(name.clone(), checkpoint);
            self.save_ledger()?;
            if previously_recorded || !network {
                self.recovered += 1;
            }
            if let Some(error) = error {
                return Err(fail(format!(
                    "batch {name} failed: {error}; outcome recorded; no automatic retry"
                )));
            }
        }
        let final_path = self.root.join("vectors.f32");
        if !exists(&final_path)? {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.root)?;
            let mut shards = Shards {
                paths: (0..self.plan.len())
                    .map(|n| {
                        self.root
                            .join("shards")
                            .join(format!("{}.f32", batch_name(n)))
                    })
                    .collect::<Vec<_>>()
                    .into_iter(),
                current: None,
            };
            PremiseVectors::write_stream(
                &self.index,
                self.identity.embedding.clone(),
                &mut shards,
                temporary.as_file_mut(),
            )?;
            temporary.as_file_mut().seek(SeekFrom::Start(0))?;
            PremiseVectors::validate_stream(
                &self.index,
                self.identity.embedding.clone(),
                temporary.as_file_mut(),
            )?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(&final_path)
                .map_err(|e| e.error)?;
            sync_parent(&final_path)?;
        }
        Ok(self.report(true))
    }

    /// Scan every durable attempt, including orphan recordings not yet present in the ledger,
    /// before filling any gap. Keep only normalized accounting, and scan past terminal outcomes
    /// so completed calls later in the plan still appear in the partial usage report.
    fn preflight_recordings(&mut self, config: &ModelConfig, network: bool) -> Result<()> {
        let mut blocked = Vec::new();
        let mut changed = false;
        let complete_cache = exists(&self.root.join("vectors.f32"))?;
        for (number, batch) in self.plan.iter().enumerate() {
            let name = batch_name(number);
            let calls = self
                .root
                .join("recordings")
                .join(&name)
                .join("attempt-0000");
            let Some(record) = only_record(&calls)? else {
                if network && exists(&calls.join("pending"))? {
                    blocked.push(format!(
                        "batch {name} has an ambiguous interrupted attempt: {}",
                        calls.display()
                    ));
                }
                if network && complete_cache {
                    blocked.push(format!(
                        "completed vector cache has missing recording evidence for {name}"
                    ));
                }
                continue;
            };
            let request = request(&self.index, &self.identity, &self.hash, number, batch);
            let mut replay = ReplayClient::from_directory(
                &calls,
                &config.generation_model,
                &config.decision_model,
            )?;
            let outcome = replay.embed(&request);
            let error = match &outcome {
                Ok(reply) => payload(
                    reply,
                    self.identity.embedding.dimensions,
                    batch.end - batch.start,
                )
                .err()
                .map(|error| error.to_string()),
                Err(error) => Some(error.to_string()),
            };
            let previous = self.ledger.batches.get(&name);
            let checkpoint = Checkpoint {
                request_sha256: batch.request_sha256.clone(),
                recording_sha256: hash_file(&record)?,
                usage: normalized_usage(&outcome),
                shard_sha256: previous.and_then(|checkpoint| checkpoint.shard_sha256.clone()),
                error: previous
                    .and_then(|checkpoint| checkpoint.error.clone())
                    .or(error),
            };
            if let Some(error) = &checkpoint.error {
                blocked.push(format!("batch {name} has a recorded failure: {error}"));
            }
            if previous != Some(&checkpoint) {
                self.ledger.batches.insert(name, checkpoint);
                changed = true;
            }
        }
        if changed {
            self.save_ledger()?;
        }
        if !blocked.is_empty() {
            return Err(fail(format!(
                "{}; no automatic network retry",
                blocked.join("; ")
            )));
        }
        Ok(())
    }

    fn save_ledger(&mut self) -> Result<()> {
        self.ledger.content_sha256 = self.ledger.checksum()?;
        replace_json(&self.root.join("ledger.json"), &self.ledger)
    }

    fn report(&self, complete: bool) -> BuildReport {
        let mut usage = UsageSummary::default();
        for checkpoint in self.ledger.batches.values() {
            usage.calls += 1;
            let part = checkpoint.usage.clone().unwrap_or_default();
            match part.input_tokens {
                Some(n) => usage.input_tokens += n,
                None => usage.unknown_input_token_calls += 1,
            }
            match part.output_tokens {
                Some(n) => usage.output_tokens += n,
                None => usage.unknown_output_token_calls += 1,
            }
            match part.cost {
                Some(n) => usage.known_cost += n,
                None => usage.unknown_cost_calls += 1,
            }
        }
        BuildReport {
            complete,
            eligible_rows: self.index.rows().len(),
            completed_batches: self
                .ledger
                .batches
                .values()
                .filter(|c| c.shard_sha256.is_some())
                .count(),
            new_calls: self.new_calls,
            recovered_batches: self.recovered,
            usage_scope: "all recorded build attempts; offline replay makes no provider call",
            provider_usage: usage,
            vector_cache: self.root.join("vectors.f32"),
            recordings: self.root.join("recordings"),
        }
    }
}

fn normalized_usage(outcome: &std::result::Result<EmbeddingReply, ModelError>) -> Option<Usage> {
    let mut usage = match outcome {
        Ok(reply) => Some(reply.metadata.usage.clone()),
        Err(ModelError::Provider { failure, .. }) => failure.usage.clone(),
        Err(_) => None,
    };
    if let Some(usage) = &mut usage {
        usage.cost = usage.cost.filter(|cost| cost.is_finite() && *cost >= 0.0);
    }
    usage
}

fn batch_name(number: usize) -> String {
    format!("batch-{number:08}")
}

fn request(
    index: &PremiseIndex,
    identity: &Identity,
    hash: &str,
    number: usize,
    batch: &Batch,
) -> EmbeddingRequest {
    EmbeddingRequest {
        context: CallContext {
            block_id: format!("retrieval-index:{hash}:{}", batch_name(number)),
            attempt: 0,
        },
        model: identity.embedding.model.clone(),
        dimensions: identity.embedding.dimensions,
        input_type: EmbeddingInputType::SearchDocument,
        input: index.rows()[batch.start..batch.end]
            .iter()
            .map(|row| row.embedding_text())
            .collect(),
    }
}

fn make_plan(index: &PremiseIndex, identity: &Identity, hash: &str) -> Result<Vec<Batch>> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < index.rows().len() {
        let limit = if start == 0 { 1 } else { identity.batch_size };
        let mut end = start;
        let mut size = 2;
        while end < index.rows().len() && end - start < limit {
            let text = index.rows()[end].embedding_text();
            let next = serde_json::to_vec(&text)?.len() + usize::from(end > start);
            if size + next > INPUT_BUDGET {
                break;
            }
            size += next;
            end += 1;
        }
        if end == start {
            return Err(fail("one embedding text exceeds serialized input budget"));
        }
        let mut batch = Batch {
            start,
            end,
            request_sha256: String::new(),
        };
        batch.request_sha256 = source_hash(&serde_json::to_vec(&request(
            index,
            identity,
            hash,
            batches.len(),
            &batch,
        ))?);
        batches.push(batch);
        start = end;
    }
    Ok(batches)
}

struct Borrowed<'a, C>(&'a mut C);
impl<C: EmbeddingClient> EmbeddingClient for Borrowed<'_, C> {
    fn embed(
        &mut self,
        request: &EmbeddingRequest,
    ) -> std::result::Result<EmbeddingReply, ModelError> {
        self.0.embed(request)
    }
}

/// Build with a caller-supplied transport; all attempts still pass through durable recording.
pub(crate) fn build_with_client(
    directory: &Path,
    config: &ModelConfig,
    batch_size: usize,
    client: &mut impl EmbeddingClient,
) -> Result<BuildReport> {
    let mut build = Build::prepare(directory, config, batch_size)?;
    let result = build.execute(config, true, |request, calls| {
        let mut recorder = RecordingClient::new(
            Borrowed(client),
            calls,
            &config.generation_model,
            &config.decision_model,
            "retrieval-index explicit live or scripted client",
        );
        let result = recorder.embed(request);
        if let Some(error) = recorder.persistence_failure() {
            if let Ok(reply) = &result {
                let _ = immutable_json(&calls.join("unpersisted-reply"), reply);
            }
            return Err(fail(error));
        }
        result.map(|_| ()).map_err(Into::into)
    });
    if result.is_err() {
        eprintln!(
            "retrieval-index partial: {}",
            serde_json::to_string(&build.report(false))?
        );
    }
    result
}

fn build_replay(
    directory: &Path,
    config: &ModelConfig,
    batch_size: usize,
    recordings: &Path,
) -> Result<BuildReport> {
    let recordings = recordings.canonicalize()?;
    let mut build = Build::prepare(directory, config, batch_size)?;
    let result = build.execute(config, false, |request, calls| {
        let name = request
            .context
            .block_id
            .rsplit(':')
            .next()
            .expect("generated context");
        let source = recordings.join(name).join("attempt-0000");
        let record = only_record(&source)?.ok_or_else(|| fail("missing replay batch recording"))?;
        // Validate the exact request before importing evidence into this build's owned directory.
        let mut replay = ReplayClient::from_directory(
            &source,
            &config.generation_model,
            &config.decision_model,
        )?;
        let result = replay.embed(request);
        if matches!(result, Err(ModelError::ReplayMismatch)) {
            return Err(fail("replay batch does not match exact request"));
        }
        create_bytes(&calls.join("call-0000.json"), &fs::read(record)?)?;
        result.map(|_| ()).map_err(Into::into)
    });
    if result.is_err() {
        eprintln!(
            "retrieval-index partial: {}",
            serde_json::to_string(&build.report(false))?
        );
    }
    result
}

fn payload(reply: &EmbeddingReply, dimensions: usize, rows: usize) -> Result<Vec<u8>> {
    if reply.embeddings.len() != rows {
        return Err(fail("embedding row count mismatch"));
    }
    let mut bytes = Vec::new();
    for row in &reply.embeddings {
        if row.len() != dimensions {
            return Err(fail("embedding dimension mismatch"));
        }
        let mut nonzero = false;
        for value in row {
            let value = *value as f32;
            if !value.is_finite() {
                return Err(fail("embedding cannot be represented as finite f32"));
            }
            nonzero |= value != 0.0;
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        if !nonzero {
            return Err(fail("zero embedding cannot define a cosine"));
        }
    }
    Ok(bytes)
}

fn only_record(directory: &Path) -> Result<Option<PathBuf>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("call-"))
        {
            return Err(fail(
                "unexpected JSON file in per-batch recording directory",
            ));
        }
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("call-"))
            && path.extension().is_some_and(|ext| ext == "json")
        {
            plain_file(&path)?;
            records.push(path);
        }
    }
    if records.len() > 1 {
        return Err(fail(
            "batch contains multiple attempts; refusing ambiguous replay",
        ));
    }
    Ok(records.pop())
}

fn validate_shard(path: &Path, rows: usize, dimensions: usize) -> Result<String> {
    let expected = rows
        .checked_mul(dimensions)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| fail("shard size overflow"))?;
    if File::open(path)?.metadata()?.len() != expected as u64 {
        return Err(fail("invalid shard length"));
    }
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; 65_536];
    let mut hasher = ContentHasher::new();
    let mut coordinate = 0;
    let mut nonzero = false;
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        if !length.is_multiple_of(4) {
            return Err(fail("invalid shard f32 alignment"));
        }
        for word in buffer[..length].as_chunks::<4>().0.iter() {
            let value = f32::from_le_bytes(*word);
            if !value.is_finite() {
                return Err(fail("nonfinite shard coordinate"));
            }
            nonzero |= value != 0.0;
            coordinate += 1;
            if coordinate == dimensions {
                if !nonzero {
                    return Err(fail("zero shard vector"));
                }
                coordinate = 0;
                nonzero = false;
            }
        }
        hasher.update(&buffer[..length]);
    }
    Ok(hasher.finish())
}

struct Shards {
    paths: std::vec::IntoIter<PathBuf>,
    current: Option<File>,
}
impl Read for Shards {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            if let Some(file) = &mut self.current {
                let count = file.read(bytes)?;
                if count > 0 {
                    return Ok(count);
                }
            }
            match self.paths.next() {
                Some(path) => self.current = Some(File::open(path)?),
                None => return Ok(0),
            }
        }
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn plain_file(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(fail(format!("expected regular file: {}", path.display())));
    }
    Ok(())
}
fn plain_dir(path: &Path) -> Result<()> {
    if !exists(path)? {
        fs::create_dir(path)?;
        sync_parent(path)?;
    }
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(fail(format!(
            "expected plain directory: {}",
            path.display()
        )));
    }
    Ok(())
}
fn probe(directory: &Path) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&[0_u8; 65_536])?;
    temporary.as_file().sync_all()?;
    Ok(())
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = [0; 65_536];
    let mut hasher = ContentHasher::new();
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finish())
}
fn create_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(
        path.parent().ok_or_else(|| fail("output has no parent"))?,
    )?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|e| e.error)?;
    sync_parent(path)?;
    Ok(())
}
fn immutable_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if exists(path)? {
        plain_file(path)?;
        if fs::read(path)? != bytes {
            return Err(fail(format!(
                "refusing incompatible existing build state: {}",
                path.display()
            )));
        }
        Ok(())
    } else {
        create_bytes(path, &bytes)
    }
}
fn replace_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if exists(path)? {
        plain_file(path)?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(
        path.parent().ok_or_else(|| fail("output has no parent"))?,
    )?;
    serde_json::to_writer_pretty(temporary.as_file_mut(), value)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|e| e.error)?;
    sync_parent(path)?;
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    File::open(path.parent().ok_or_else(|| fail("output has no parent"))?)?.sync_all()?;
    Ok(())
}
