//! dragon-cli::indexer - Motor unificado de indexación incremental, checkpoints y validación de arranque.
//!
//! Funcionalidades:
//! 1. Indexación automática inicial de todas las unidades locales (omitiendo archivos/carpetas del SO).
//! 2. Checkpoints persistentes en `redb` cada N archivos y por carpeta objetivo, permitiendo reanudar
//!    exactamente desde el último punto de control si la herramienta se detiene.
//! 3. Validación completa en cada arranque: detecta archivos nuevos, modificados por peso/fecha,
//!    eliminados y archivos movidos de su ubicación original (conciliación por nombre + peso + fecha).

use anyhow::Result;
use dragon_ai::{DragonEmbedder, VectorStore};
use dragon_core::{classify_path, FileCategory, FileRecord};
use dragon_extract::{detect_format, extract_content, DocumentFormat};
use dragon_index::{DragonIndexEngine, IndexWriter};
use dragon_mft::{discover_local_user_targets, UnifiedScanResult, UnifiedScanner};
use dragon_storage::{CheckpointState, DragonStorage, IndexedFileState};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Cada cuántos cambios (archivos nuevos/modificados/movidos) se guarda un checkpoint firme a disco.
const CHECKPOINT_BATCH_SIZE: usize = 250;

/// Estado vivo compartido con el servidor HTTP del HUD para mostrar progreso en tiempo real.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexerTelemetry {
    pub phase: String,
    pub current_target: String,
    pub target_index: usize,
    pub total_targets: usize,
    pub new_files: usize,
    pub updated_files: usize,
    pub moved_files: usize,
    pub deleted_files: usize,
    pub skipped_files: usize,
    pub last_checkpoint: String,
    pub initial_scan_completed: bool,
}

/// Recursos compartidos entre el servidor HTTP del HUD y el worker de indexación en segundo plano.
pub struct SharedRadarState {
    pub storage: Arc<DragonStorage>,
    pub index_engine: Arc<DragonIndexEngine>,
    pub embedder: Arc<DragonEmbedder>,
    pub vector_store: Arc<RwLock<VectorStore>>,
    pub telemetry: Arc<RwLock<IndexerTelemetry>>,
    pub vectors_file: PathBuf,
}

impl SharedRadarState {
    pub fn open(index_dir: &Path, db_path: &Path, vectors_file: &Path) -> Result<Arc<Self>> {
        let storage = Arc::new(DragonStorage::open(db_path)?);
        let index_engine = Arc::new(DragonIndexEngine::open_or_create(index_dir)?);
        let embedder = Arc::new(DragonEmbedder::load_or_detect::<&str>(None));
        let vector_store = Arc::new(RwLock::new(
            VectorStore::load_from_file(vectors_file).unwrap_or_default(),
        ));
        let cp = storage.load_checkpoint().unwrap_or_default();

        let telemetry = Arc::new(RwLock::new(IndexerTelemetry {
            phase: if cp.initial_scan_completed && !cp.in_progress {
                "READY".to_string()
            } else {
                "PENDING".to_string()
            },
            initial_scan_completed: cp.initial_scan_completed,
            last_checkpoint: cp.last_checkpoint_path.unwrap_or_default(),
            ..Default::default()
        }));

        Ok(Arc::new(Self {
            storage,
            index_engine,
            embedder,
            vector_store,
            telemetry,
            vectors_file: vectors_file.to_path_buf(),
        }))
    }
}

#[derive(Debug, Clone, Default)]
pub struct SyncSummary {
    pub new_files: usize,
    pub updated_files: usize,
    pub moved_files: usize,
    pub deleted_files: usize,
    pub skipped_files: usize,
    pub stored_records: usize,
    pub resumed_from_checkpoint: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MoveSignature {
    name_lower: String,
    size: u64,
    modified_ms: u64,
}

struct MissingEntry {
    key: String,
    orig_path: String,
    state: IndexedFileState,
}

/// Flujo principal al iniciar la aplicación:
/// - Primera vez o sesión interrumpida: ejecuta indexación automática con checkpoints persistentes.
/// - Arranques posteriores: valida el índice completo (nuevos, modificados por peso/fecha, movidos y eliminados).
pub fn run_startup_auto_sync_and_validate(
    state: &Arc<SharedRadarState>,
    force_full: bool,
) -> Result<SyncSummary> {
    let t0 = Instant::now();
    let mut summary = SyncSummary::default();
    let mut cp = state.storage.load_checkpoint().unwrap_or_default();

    let needs_initial_or_resume = force_full || !cp.initial_scan_completed || cp.in_progress;

    if needs_initial_or_resume {
        let resuming = !force_full && cp.in_progress && !cp.targets.is_empty();
        summary.resumed_from_checkpoint = resuming;

        if !resuming {
            let discovered: Vec<String> = discover_local_user_targets(false)
                .into_iter()
                .map(|p| clean_abs_path(&p).to_string_lossy().to_string())
                .collect();

            cp = CheckpointState {
                initial_scan_completed: false,
                in_progress: true,
                targets: discovered,
                current_target_idx: 0,
                last_checkpoint_path: None,
                processed_count: 0,
                updated_at_ms: now_epoch_ms(),
            };
            state.storage.save_checkpoint(&cp)?;
            log::info!(
                "[CHECKPOINT] Iniciando indexación automática inicial de {} ubicaciones locales...",
                cp.targets.len()
            );
        } else {
            log::info!(
                "[CHECKPOINT] Reanudando indexación desde punto de control {}/{} ({})",
                cp.current_target_idx + 1,
                cp.targets.len(),
                cp.last_checkpoint_path.as_deref().unwrap_or("-")
            );
        }

        let phase_name = if resuming {
            "RESUMING_CHECKPOINT"
        } else {
            "INITIAL_INDEXING"
        };
        update_telemetry(state, |t| {
            t.phase = phase_name.to_string();
            t.total_targets = cp.targets.len();
            t.target_index = cp.current_target_idx;
            t.initial_scan_completed = false;
        });

        let mut writer = state.index_engine.create_writer(64)?;
        let total_targets = cp.targets.len();

        while cp.current_target_idx < total_targets {
            let idx = cp.current_target_idx;
            let target = cp.targets[idx].clone();

            update_telemetry(state, |t| {
                t.target_index = idx + 1;
                t.current_target = target.clone();
            });

            if Path::new(&target).exists() {
                let target_sum = index_target_incremental_internal(
                    state,
                    &mut writer,
                    &target,
                    force_full,
                    false, // Durante el barrido por partes, conciliamos movimientos/borrados al cierre global
                    Some(&mut cp),
                )?;

                summary.new_files += target_sum.new_files;
                summary.updated_files += target_sum.updated_files;
                summary.skipped_files += target_sum.skipped_files;
                summary.stored_records += target_sum.stored_records;
            }

            cp.current_target_idx = idx + 1;
            cp.last_checkpoint_path = Some(target);
            cp.updated_at_ms = now_epoch_ms();
            state.storage.save_checkpoint(&cp)?;

            update_telemetry(state, |t| {
                t.new_files = summary.new_files;
                t.updated_files = summary.updated_files;
                t.skipped_files = summary.skipped_files;
                t.last_checkpoint = cp.last_checkpoint_path.clone().unwrap_or_default();
            });
        }

        // Conciliación final de archivos movidos o borrados tras completar las unidades
        let reconcile_sum = validate_and_reconcile_global(state, &mut writer, &cp.targets)?;
        summary.moved_files += reconcile_sum.moved_files;
        summary.deleted_files += reconcile_sum.deleted_files;
        summary.new_files += reconcile_sum.new_files;
        summary.updated_files += reconcile_sum.updated_files;

        writer.commit()?;
        let _ = state.index_engine.reload();
        save_vector_store_to_disk(state);

        cp.initial_scan_completed = true;
        cp.in_progress = false;
        cp.current_target_idx = 0;
        cp.last_checkpoint_path = Some("COMPLETADO".to_string());
        cp.updated_at_ms = now_epoch_ms();
        state.storage.save_checkpoint(&cp)?;
    } else {
        // Arranque normal: validación global de índice (nuevos, modificados por peso/fecha, movidos, eliminados)
        update_telemetry(state, |t| {
            t.phase = "VALIDATING_INDEX".to_string();
            t.initial_scan_completed = true;
        });

        let targets: Vec<String> = discover_local_user_targets(false)
            .into_iter()
            .map(|p| clean_abs_path(&p).to_string_lossy().to_string())
            .collect();

        let mut writer = state.index_engine.create_writer(64)?;
        summary = validate_and_reconcile_global(state, &mut writer, &targets)?;

        if summary.new_files + summary.updated_files + summary.moved_files + summary.deleted_files > 0 {
            writer.commit()?;
            let _ = state.index_engine.reload();
            save_vector_store_to_disk(state);
        }

        cp.updated_at_ms = now_epoch_ms();
        cp.last_checkpoint_path = Some(format!(
            "Validado (+{} ~{} >{} -{})",
            summary.new_files, summary.updated_files, summary.moved_files, summary.deleted_files
        ));
        state.storage.save_checkpoint(&cp)?;
    }

    update_telemetry(state, |t| {
        t.phase = "READY".to_string();
        t.new_files = summary.new_files;
        t.updated_files = summary.updated_files;
        t.moved_files = summary.moved_files;
        t.deleted_files = summary.deleted_files;
        t.skipped_files = summary.skipped_files;
        t.initial_scan_completed = true;
    });

    log::info!(
        "Sincronización finalizada en {:?} -> nuevos: {}, modificados: {}, movidos: {}, eliminados: {}, omitidos: {}",
        t0.elapsed(),
        summary.new_files,
        summary.updated_files,
        summary.moved_files,
        summary.deleted_files,
        summary.skipped_files
    );

    Ok(summary)
}

/// Indexa una sola ruta/carpeta específica de forma incremental con soporte de checkpoints por lote.
pub fn index_single_target(
    state: &Arc<SharedRadarState>,
    target: &str,
    force: bool,
) -> Result<SyncSummary> {
    let clean_target = clean_abs_path(Path::new(target)).to_string_lossy().to_string();
    let mut writer = state.index_engine.create_writer(64)?;
    let sum = index_target_incremental_internal(state, &mut writer, &clean_target, force, true, None)?;
    if sum.new_files + sum.updated_files + sum.moved_files + sum.deleted_files > 0 {
        writer.commit()?;
        let _ = state.index_engine.reload();
        save_vector_store_to_disk(state);
    }
    Ok(sum)
}

fn index_target_incremental_internal(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    clean_target_str: &str,
    force: bool,
    reconcile_moves_and_deletions: bool,
    mut checkpoint_opt: Option<&mut CheckpointState>,
) -> Result<SyncSummary> {
    let existing_states = state.storage.load_index_states()?;
    let scan_res = UnifiedScanner::scan(clean_target_str).map_err(|e| anyhow::anyhow!(e))?;
    let now_ms = now_epoch_ms();

    let mut summary = SyncSummary::default();
    let mut seen_keys = HashSet::new();
    let mut candidate_new: Vec<FileRecord> = Vec::new();
    let mut batch_records: Vec<FileRecord> = Vec::with_capacity(500);
    let mut batch_states: Vec<(String, IndexedFileState)> = Vec::with_capacity(500);
    let mut pending_changes_since_cp = 0usize;
    let mut vectors_dirty = false;

    let records = collect_scan_records(scan_res);

    for mut rec in records {
        let Some(raw_path) = rec.path.take() else {
            continue;
        };
        let clean_path = clean_abs_path(&raw_path);
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);
        rec.path = Some(clean_path.clone());

        seen_keys.insert(path_key.clone());

        if rec.is_directory {
            if force || !existing_states.contains_key(&path_key) {
                batch_states.push((
                    path_key,
                    IndexedFileState {
                        doc_id: rec.id,
                        size: 0,
                        modified_ms: rec.modified_ms,
                        last_indexed_ms: now_ms,
                        is_dir: true,
                        original_path: path_str,
                    },
                ));
                batch_records.push(rec);
                if batch_records.len() >= 500 {
                    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                }
            }
            continue;
        }

        if let Some(prev) = existing_states.get(&path_key) {
            if !force && prev.size == rec.size && prev.modified_ms == rec.modified_ms {
                summary.skipped_files += 1;
                continue;
            }
            // Modificado por peso o fecha (o forzado)
            process_extract_and_write(
                state,
                writer,
                &rec,
                &clean_path,
                &path_str,
                &mut vectors_dirty,
            );
            summary.updated_files += 1;
            pending_changes_since_cp += 1;

            batch_states.push((
                path_key,
                IndexedFileState {
                    doc_id: rec.id,
                    size: rec.size,
                    modified_ms: rec.modified_ms,
                    last_indexed_ms: now_ms,
                    is_dir: false,
                    original_path: path_str.clone(),
                },
            ));
            batch_records.push(rec);
        } else {
            // Candidato a nuevo o movido
            candidate_new.push(rec);
        }

        if pending_changes_since_cp >= CHECKPOINT_BATCH_SIZE {
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            writer.commit()?;
            let _ = state.index_engine.reload();
            if vectors_dirty {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
            }
            if let Some(ref mut cp) = checkpoint_opt {
                cp.processed_count += pending_changes_since_cp;
                cp.last_checkpoint_path = Some(path_str.clone());
                cp.updated_at_ms = now_epoch_ms();
                let _ = state.storage.save_checkpoint(cp);
            }
            pending_changes_since_cp = 0;
        }
    }

    // Buscar entradas faltantes dentro del árbol escaneado si corresponde conciliar movimientos/borrados
    let mut unmatched_missing: Vec<MissingEntry> = Vec::new();
    let mut missing_by_sig: HashMap<MoveSignature, Vec<MissingEntry>> = HashMap::new();

    if reconcile_moves_and_deletions {
        let target_key = normalize_path_key(clean_target_str);
        let target_prefix = ensure_dir_prefix(&target_key);

        for (key, st) in &existing_states {
            let inside_target = key == &target_key || key.starts_with(&target_prefix);
            if inside_target && !seen_keys.contains(key) {
                let orig_path = resolve_orig_path(&state.storage, key, st);
                if st.is_dir {
                    unmatched_missing.push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                    continue;
                }
                if st.size > 0 && st.modified_ms > 0 {
                    let fname_lower = Path::new(&orig_path)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let sig = MoveSignature {
                        name_lower: fname_lower,
                        size: st.size,
                        modified_ms: st.modified_ms,
                    };
                    missing_by_sig.entry(sig).or_default().push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                } else {
                    unmatched_missing.push(MissingEntry {
                        key: key.clone(),
                        orig_path,
                        state: st.clone(),
                    });
                }
            }
        }
    }

    // Procesar candidate_new: conciliar si fue MOVIDO desde otro punto, si no es NUEVO
    let mut moved_removals: Vec<(String, String, u64)> = Vec::new();
    for rec in candidate_new {
        let Some(ref clean_path) = rec.path else {
            continue;
        };
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);

        let moved_from = if reconcile_moves_and_deletions && rec.size > 0 && rec.modified_ms > 0 {
            let sig = MoveSignature {
                name_lower: rec.name.to_lowercase(),
                size: rec.size,
                modified_ms: rec.modified_ms,
            };
            missing_by_sig.get_mut(&sig).and_then(|list| list.pop())
        } else {
            None
        };

        if let Some(old_entry) = moved_from {
            handle_moved_file(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &old_entry.orig_path,
                &mut vectors_dirty,
            );
            moved_removals.push((old_entry.key, old_entry.orig_path, old_entry.state.doc_id));
            summary.moved_files += 1;
        } else {
            process_extract_and_write(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &mut vectors_dirty,
            );
            summary.new_files += 1;
        }

        pending_changes_since_cp += 1;
        batch_states.push((
            path_key,
            IndexedFileState {
                doc_id: rec.id,
                size: rec.size,
                modified_ms: rec.modified_ms,
                last_indexed_ms: now_ms,
                is_dir: false,
                original_path: path_str.clone(),
            },
        ));
        batch_records.push(rec);

        if pending_changes_since_cp >= CHECKPOINT_BATCH_SIZE {
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            if !moved_removals.is_empty() {
                let _ = state.storage.remove_by_paths(&moved_removals);
                moved_removals.clear();
            }
            writer.commit()?;
            let _ = state.index_engine.reload();
            if vectors_dirty {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
            }
            if let Some(ref mut cp) = checkpoint_opt {
                cp.processed_count += pending_changes_since_cp;
                cp.last_checkpoint_path = Some(path_str);
                cp.updated_at_ms = now_epoch_ms();
                let _ = state.storage.save_checkpoint(cp);
            }
            pending_changes_since_cp = 0;
        }
    }

    // Purgar archivos eliminados (lo que no coincidió como movido)
    for (_, list) in missing_by_sig {
        unmatched_missing.extend(list);
    }

    if !unmatched_missing.is_empty() {
        let mut to_remove = Vec::with_capacity(unmatched_missing.len());
        for entry in unmatched_missing {
            state.index_engine.delete_by_path(writer, &entry.orig_path);
            if let Ok(mut vs) = state.vector_store.write() {
                if vs.remove_by_path(&entry.orig_path) {
                    vectors_dirty = true;
                }
            }
            to_remove.push((entry.key, entry.orig_path, entry.state.doc_id));
            summary.deleted_files += 1;
        }
        state.storage.remove_by_paths(&to_remove)?;
    }

    if !moved_removals.is_empty() {
        state.storage.remove_by_paths(&moved_removals)?;
    }

    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
    if vectors_dirty {
        save_vector_store_to_disk(state);
    }

    Ok(summary)
}

/// Validación global de arranque a través de todas las unidades locales del equipo:
/// verifica archivos nuevos, modificados (peso/fecha), eliminados y movidos entre cualquier carpeta/unidad.
fn validate_and_reconcile_global(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    targets: &[String],
) -> Result<SyncSummary> {
    let existing_states = state.storage.load_index_states()?;
    let now_ms = now_epoch_ms();

    let mut summary = SyncSummary::default();
    let mut seen_keys = HashSet::new();
    let mut candidate_new: Vec<FileRecord> = Vec::new();
    let mut batch_records: Vec<FileRecord> = Vec::with_capacity(500);
    let mut batch_states: Vec<(String, IndexedFileState)> = Vec::with_capacity(500);
    let mut pending_changes = 0usize;
    let mut vectors_dirty = false;

    let total_targets = targets.len();
    update_telemetry(state, |t| {
        t.total_targets = total_targets;
    });

    for (idx, target) in targets.iter().enumerate() {
        if !Path::new(target).exists() {
            continue;
        }
        update_telemetry(state, |t| {
            t.target_index = idx + 1;
            t.current_target = target.clone();
        });

        let Ok(scan_res) = UnifiedScanner::scan(target) else {
            continue;
        };
        let records = collect_scan_records(scan_res);

        for mut rec in records {
            let Some(raw_path) = rec.path.take() else {
                continue;
            };
            let clean_path = clean_abs_path(&raw_path);
            let path_str = clean_path.to_string_lossy().to_string();
            let path_key = normalize_path_key(&path_str);
            rec.path = Some(clean_path.clone());

            seen_keys.insert(path_key.clone());

            if rec.is_directory {
                if !existing_states.contains_key(&path_key) {
                    batch_states.push((
                        path_key,
                        IndexedFileState {
                            doc_id: rec.id,
                            size: 0,
                            modified_ms: rec.modified_ms,
                            last_indexed_ms: now_ms,
                            is_dir: true,
                            original_path: path_str,
                        },
                    ));
                    batch_records.push(rec);
                    if batch_records.len() >= 500 {
                        summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                    }
                }
                continue;
            }

            if let Some(prev) = existing_states.get(&path_key) {
                if prev.size == rec.size && prev.modified_ms == rec.modified_ms {
                    summary.skipped_files += 1;
                    continue;
                }
                // Modificado por tamaño o fecha
                process_extract_and_write(
                    state,
                    writer,
                    &rec,
                    &clean_path,
                    &path_str,
                    &mut vectors_dirty,
                );
                summary.updated_files += 1;
                pending_changes += 1;

                batch_states.push((
                    path_key,
                    IndexedFileState {
                        doc_id: rec.id,
                        size: rec.size,
                        modified_ms: rec.modified_ms,
                        last_indexed_ms: now_ms,
                        is_dir: false,
                        original_path: path_str,
                    },
                ));
                batch_records.push(rec);
            } else {
                candidate_new.push(rec);
            }

            if pending_changes >= CHECKPOINT_BATCH_SIZE {
                summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
                writer.commit()?;
                let _ = state.index_engine.reload();
                if vectors_dirty {
                    save_vector_store_to_disk(state);
                    vectors_dirty = false;
                }
                pending_changes = 0;
            }
        }
    }

    // Verificar todas las entradas no vistas en disco para detectar movidos vs eliminados reales
    let mut unmatched_missing: Vec<MissingEntry> = Vec::new();
    let mut missing_by_sig: HashMap<MoveSignature, Vec<MissingEntry>> = HashMap::new();

    for (key, st) in &existing_states {
        if seen_keys.contains(key) {
            continue;
        }
        let orig_path = resolve_orig_path(&state.storage, key, st);
        // Comprobación física: si aún existe en disco (ej. carpeta personalizada agregada manualmente), no borrar
        if Path::new(&orig_path).exists() {
            continue;
        }
        if st.is_dir {
            unmatched_missing.push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
            continue;
        }

        if st.size > 0 && st.modified_ms > 0 {
            let fname_lower = Path::new(&orig_path)
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let sig = MoveSignature {
                name_lower: fname_lower,
                size: st.size,
                modified_ms: st.modified_ms,
            };
            missing_by_sig.entry(sig).or_default().push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
        } else {
            unmatched_missing.push(MissingEntry {
                key: key.clone(),
                orig_path,
                state: st.clone(),
            });
        }
    }

    // Conciliar candidate_new con missing_by_sig (detección global de archivos movidos entre carpetas/unidades)
    let mut moved_removals: Vec<(String, String, u64)> = Vec::new();
    for rec in candidate_new {
        let Some(ref clean_path) = rec.path else {
            continue;
        };
        let path_str = clean_path.to_string_lossy().to_string();
        let path_key = normalize_path_key(&path_str);

        let moved_from = if rec.size > 0 && rec.modified_ms > 0 {
            let sig = MoveSignature {
                name_lower: rec.name.to_lowercase(),
                size: rec.size,
                modified_ms: rec.modified_ms,
            };
            missing_by_sig.get_mut(&sig).and_then(|list| list.pop())
        } else {
            None
        };

        if let Some(old_entry) = moved_from {
            log::info!("[MOVED] {} -> {}", old_entry.orig_path, path_str);
            handle_moved_file(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &old_entry.orig_path,
                &mut vectors_dirty,
            );
            moved_removals.push((old_entry.key, old_entry.orig_path, old_entry.state.doc_id));
            summary.moved_files += 1;
        } else {
            process_extract_and_write(
                state,
                writer,
                &rec,
                clean_path,
                &path_str,
                &mut vectors_dirty,
            );
            summary.new_files += 1;
        }

        pending_changes += 1;
        batch_states.push((
            path_key,
            IndexedFileState {
                doc_id: rec.id,
                size: rec.size,
                modified_ms: rec.modified_ms,
                last_indexed_ms: now_ms,
                is_dir: false,
                original_path: path_str,
            },
        ));
        batch_records.push(rec);

        if pending_changes >= CHECKPOINT_BATCH_SIZE {
            summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
            if !moved_removals.is_empty() {
                let _ = state.storage.remove_by_paths(&moved_removals);
                moved_removals.clear();
            }
            writer.commit()?;
            let _ = state.index_engine.reload();
            if vectors_dirty {
                save_vector_store_to_disk(state);
                vectors_dirty = false;
            }
            pending_changes = 0;
        }
    }

    for (_, list) in missing_by_sig {
        unmatched_missing.extend(list);
    }

    if !unmatched_missing.is_empty() {
        let mut to_remove = Vec::with_capacity(unmatched_missing.len());
        for entry in unmatched_missing {
            log::debug!("[DELETED] {}", entry.orig_path);
            state.index_engine.delete_by_path(writer, &entry.orig_path);
            if let Ok(mut vs) = state.vector_store.write() {
                if vs.remove_by_path(&entry.orig_path) {
                    vectors_dirty = true;
                }
            }
            to_remove.push((entry.key, entry.orig_path, entry.state.doc_id));
            summary.deleted_files += 1;
        }
        state.storage.remove_by_paths(&to_remove)?;
    }

    if !moved_removals.is_empty() {
        state.storage.remove_by_paths(&moved_removals)?;
    }

    summary.stored_records += flush_redb_batches(&state.storage, &mut batch_records, &mut batch_states)?;
    if vectors_dirty {
        save_vector_store_to_disk(state);
    }

    Ok(summary)
}

fn handle_moved_file(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    rec: &FileRecord,
    clean_path: &Path,
    new_path_str: &str,
    old_path_str: &str,
    vectors_dirty: &mut bool,
) {
    state.index_engine.delete_by_path(writer, old_path_str);

    if classify_path(clean_path) != FileCategory::Document {
        if let Ok(mut vs) = state.vector_store.write() {
            if vs.remove_by_path(old_path_str) {
                *vectors_dirty = true;
            }
        }
        return;
    }

    if let Ok(mut vs) = state.vector_store.write() {
        if vs.relocate_path(old_path_str, new_path_str, rec.id) {
            *vectors_dirty = true;
        }
    }

    let format = detect_format(clean_path);
    let content = if format != DocumentFormat::Unsupported {
        extract_content(clean_path, 50_000).unwrap_or_default()
    } else {
        String::new()
    };
    let _ = state.index_engine.add_document(
        writer,
        rec.id,
        new_path_str,
        &rec.name,
        &content,
        rec.size,
        rec.modified_ms,
    );
}

fn process_extract_and_write(
    state: &Arc<SharedRadarState>,
    writer: &mut IndexWriter,
    rec: &FileRecord,
    clean_path: &Path,
    path_str: &str,
    vectors_dirty: &mut bool,
) {
    // Regla estricta:
    // - Documentos conocidos -> Nombre (Redb) + Contenido (Tantivy) + Vectores (Model2Vec)
    // - Multimedia, Imágenes/Diseño, Sistema/Binarios -> Únicamente por Nombre (Redb)
    if classify_path(clean_path) != FileCategory::Document {
        state.index_engine.delete_by_path(writer, path_str);
        if let Ok(mut vs) = state.vector_store.write() {
            if vs.remove_by_path(path_str) {
                *vectors_dirty = true;
            }
        }
        return;
    }

    let format = detect_format(clean_path);
    let content = if format != DocumentFormat::Unsupported {
        extract_content(clean_path, 50_000).unwrap_or_default()
    } else {
        String::new()
    };

    let _ = state.index_engine.add_document(
        writer,
        rec.id,
        path_str,
        &rec.name,
        &content,
        rec.size,
        rec.modified_ms,
    );

    if !content.is_empty() {
        let snippet: String = content.chars().take(800).collect();
        let vec = state.embedder.embed(&snippet);
        if let Ok(mut vs) = state.vector_store.write() {
            vs.upsert(rec.id, path_str, 0, &snippet, &vec);
            *vectors_dirty = true;
        }
    } else if let Ok(mut vs) = state.vector_store.write() {
        if vs.remove_by_path(path_str) {
            *vectors_dirty = true;
        }
    }
}

fn collect_scan_records(scan_res: UnifiedScanResult) -> Vec<FileRecord> {
    match scan_res {
        #[cfg(windows)]
        UnifiedScanResult::Mft(mft) => {
            let mut out = Vec::with_capacity(mft.records.len());
            for (&id, &(parent_id, ref name, attr)) in &mft.records {
                let is_dir = (attr & FileRecord::ATTR_DIRECTORY) != 0;
                if let Some(resolved_path) = mft.resolve_path(id) {
                    let (size, modified_ms) = if is_dir {
                        (0, 0)
                    } else {
                        read_size_and_mtime(&resolved_path)
                    };
                    out.push(FileRecord {
                        id,
                        parent_id,
                        name: name.clone(),
                        path: Some(resolved_path),
                        is_directory: is_dir,
                        size,
                        modified_ms,
                        attributes: attr,
                    });
                }
            }
            out
        }
        UnifiedScanResult::Fallback(fallback) => fallback.records,
    }
}

fn flush_redb_batches(
    storage: &DragonStorage,
    batch_records: &mut Vec<FileRecord>,
    batch_states: &mut Vec<(String, IndexedFileState)>,
) -> Result<usize> {
    let mut stored = 0;
    if !batch_records.is_empty() {
        stored = storage.insert_batch(batch_records)?;
        batch_records.clear();
    }
    if !batch_states.is_empty() {
        storage.upsert_index_states(batch_states)?;
        batch_states.clear();
    }
    Ok(stored)
}

fn save_vector_store_to_disk(state: &Arc<SharedRadarState>) {
    if let Some(parent) = state.vectors_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(vs) = state.vector_store.read() {
        let _ = vs.save_to_file(&state.vectors_file);
    }
}

fn resolve_orig_path(storage: &DragonStorage, key: &str, st: &IndexedFileState) -> String {
    if !st.original_path.is_empty() {
        return st.original_path.clone();
    }
    storage
        .get_by_id(st.doc_id)
        .ok()
        .flatten()
        .and_then(|r| r.path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| key.to_string())
}

fn update_telemetry<F: FnOnce(&mut IndexerTelemetry)>(state: &Arc<SharedRadarState>, f: F) {
    if let Ok(mut t) = state.telemetry.write() {
        f(&mut t);
    }
}

pub fn clean_abs_path(path: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|c| c.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    });
    let s = abs.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", stripped))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        abs
    }
}

pub fn normalize_path_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_string()
    }
}

fn ensure_dir_prefix(key: &str) -> String {
    if key.ends_with('\\') || key.ends_with('/') {
        key.to_string()
    } else {
        format!("{}{}", key, std::path::MAIN_SEPARATOR)
    }
}

pub fn read_size_and_mtime(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let size = m.len();
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (size, mtime)
        }
        Err(_) => (0, 0),
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
