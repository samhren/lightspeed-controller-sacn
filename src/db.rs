use rusqlite::{Connection, params};
use crate::model::*;
use std::path::Path;
use anyhow::{Result, Context};
use std::collections::HashMap;

/// Database connection manager for Lightspeed configuration
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create database at the specified path
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database at {:?}", path))?;

        // Enable WAL mode for better concurrency
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Initialize database schema
    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS strips (
                id INTEGER PRIMARY KEY,
                universe INTEGER NOT NULL,
                start_channel INTEGER NOT NULL,
                pixel_count INTEGER NOT NULL,
                x REAL NOT NULL,
                y REAL NOT NULL,
                spacing REAL NOT NULL,
                flipped INTEGER NOT NULL DEFAULT 0,
                color_order TEXT NOT NULL DEFAULT 'RGB'
            );
            CREATE INDEX IF NOT EXISTS idx_strips_universe ON strips(universe);

            CREATE TABLE IF NOT EXISTS masks (
                id INTEGER PRIMARY KEY,
                mask_type TEXT NOT NULL,
                x REAL NOT NULL,
                y REAL NOT NULL,
                params_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scenes (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                global_effect_json TEXT,
                launchpad_btn INTEGER,
                launchpad_is_cc INTEGER NOT NULL DEFAULT 0,
                launchpad_color INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_scenes_name ON scenes(name);

            CREATE TABLE IF NOT EXISTS scene_masks (
                scene_id INTEGER NOT NULL,
                mask_id INTEGER NOT NULL,
                mask_type TEXT NOT NULL,
                x REAL NOT NULL,
                y REAL NOT NULL,
                params_json TEXT NOT NULL,
                display_order INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (scene_id, mask_id),
                FOREIGN KEY (scene_id) REFERENCES scenes(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS app_config (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                selected_scene_id INTEGER,
                network_use_multicast INTEGER NOT NULL DEFAULT 1,
                network_unicast_ip TEXT NOT NULL DEFAULT '192.168.1.50',
                network_universe INTEGER NOT NULL DEFAULT 1,
                bind_address TEXT,
                mode TEXT NOT NULL DEFAULT '',
                effect TEXT NOT NULL DEFAULT '',
                audio_latency_ms REAL NOT NULL DEFAULT 0.0,
                audio_use_flywheel INTEGER NOT NULL DEFAULT 1,
                audio_hybrid_sync INTEGER NOT NULL DEFAULT 0,
                audio_sensitivity REAL NOT NULL DEFAULT 0.5,
                layout_locked INTEGER NOT NULL DEFAULT 0,
                FOREIGN KEY (selected_scene_id) REFERENCES scenes(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            INSERT OR IGNORE INTO metadata (key, value) VALUES ('schema_version', '1');
            INSERT OR IGNORE INTO metadata (key, value) VALUES ('migrated_from_json', '0');
            INSERT OR IGNORE INTO app_config (id) VALUES (1);
            "#
        )?;
        Ok(())
    }

    /// Check if migration from JSON is needed
    pub fn needs_migration(&self) -> Result<bool> {
        let migrated: String = self.conn.query_row(
            "SELECT value FROM metadata WHERE key = 'migrated_from_json'",
            [],
            |row| row.get(0)
        )?;
        Ok(migrated == "0")
    }

    /// Mark migration as complete
    pub fn mark_migration_complete(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE metadata SET value = '1' WHERE key = 'migrated_from_json'",
            []
        )?;
        Ok(())
    }

    /// Migrate from JSON AppState to SQLite
    pub fn migrate_from_json(&mut self, state: &AppState) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Clear existing data
        tx.execute("DELETE FROM scene_masks", [])?;
        tx.execute("DELETE FROM scenes", [])?;
        tx.execute("DELETE FROM masks", [])?;
        tx.execute("DELETE FROM strips", [])?;

        // Migrate strips
        for strip in &state.strips {
            tx.execute(
                "INSERT INTO strips (id, universe, start_channel, pixel_count, x, y, spacing, flipped, color_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    strip.id as i64,
                    strip.universe,
                    strip.start_channel,
                    strip.pixel_count,
                    strip.x,
                    strip.y,
                    strip.spacing,
                    if strip.flipped { 1 } else { 0 },
                    strip.color_order,
                ],
            )?;
        }

        // Migrate global masks
        for mask in &state.masks {
            let params_json = serde_json::to_string(&mask.params)?;
            tx.execute(
                "INSERT INTO masks (id, mask_type, x, y, params_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![mask.id as i64, mask.mask_type, mask.x, mask.y, params_json],
            )?;
        }

        // Migrate scenes
        for scene in &state.scenes {
            let global_effect_json = scene.global.as_ref()
                .map(|g| serde_json::to_string(g))
                .transpose()?;

            tx.execute(
                "INSERT INTO scenes (id, name, kind, global_effect_json, launchpad_btn, launchpad_is_cc, launchpad_color)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    scene.id as i64,
                    scene.name,
                    scene.kind,
                    global_effect_json,
                    scene.launchpad_btn.map(|v| v as i64),
                    if scene.launchpad_is_cc { 1 } else { 0 },
                    scene.launchpad_color.map(|v| v as i64),
                ],
            )?;

            // Migrate scene masks
            for (idx, mask) in scene.masks.iter().enumerate() {
                let params_json = serde_json::to_string(&mask.params)?;
                tx.execute(
                    "INSERT INTO scene_masks (scene_id, mask_id, mask_type, x, y, params_json, display_order)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        scene.id as i64,
                        mask.id as i64,
                        mask.mask_type,
                        mask.x,
                        mask.y,
                        params_json,
                        idx as i64,
                    ],
                )?;
            }
        }

        // Migrate app config
        tx.execute(
            "UPDATE app_config SET
                selected_scene_id = ?1,
                network_use_multicast = ?2,
                network_unicast_ip = ?3,
                network_universe = ?4,
                bind_address = ?5,
                mode = ?6,
                effect = ?7,
                audio_latency_ms = ?8,
                audio_use_flywheel = ?9,
                audio_hybrid_sync = ?10,
                audio_sensitivity = ?11,
                layout_locked = ?12
             WHERE id = 1",
            params![
                state.selected_scene_id,
                if state.network.use_multicast { 1 } else { 0 },
                state.network.unicast_ip,
                state.network.universe,
                state.bind_address,
                state.mode,
                state.effect,
                state.audio.latency_ms,
                if state.audio.use_flywheel { 1 } else { 0 },
                if state.audio.hybrid_sync { 1 } else { 0 },
                state.audio.sensitivity,
                if state.layout_locked { 1 } else { 0 },
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Load entire app state from database
    pub fn load_state(&self) -> Result<AppState> {
        // Load strips
        let mut stmt = self.conn.prepare(
            "SELECT id, universe, start_channel, pixel_count, x, y, spacing, flipped, color_order FROM strips ORDER BY id"
        )?;
        let strips = stmt.query_map([], |row| {
            let pixel_count: usize = row.get(3)?;
            Ok(PixelStrip {
                id: row.get::<_, i64>(0)? as u64,
                universe: row.get(1)?,
                start_channel: row.get(2)?,
                pixel_count,
                x: row.get(4)?,
                y: row.get(5)?,
                spacing: row.get(6)?,
                flipped: row.get::<_, i64>(7)? != 0,
                color_order: row.get(8)?,
                data: vec![[0, 0, 0]; pixel_count], // Initialize with black pixels
            })
        })?.collect::<Result<Vec<_>, _>>()?;

        // Load global masks
        let mut stmt = self.conn.prepare(
            "SELECT id, mask_type, x, y, params_json FROM masks ORDER BY id"
        )?;
        let masks = stmt.query_map([], |row| {
            let params_json: String = row.get(4)?;
            let params: HashMap<String, serde_json::Value> = serde_json::from_str(&params_json)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

            Ok(Mask {
                id: row.get::<_, i64>(0)? as u64,
                mask_type: row.get(1)?,
                x: row.get(2)?,
                y: row.get(3)?,
                params,
            })
        })?.collect::<Result<Vec<_>, _>>()?;

        // Load scenes
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, global_effect_json, launchpad_btn, launchpad_is_cc, launchpad_color FROM scenes ORDER BY id"
        )?;
        let scene_rows: Vec<_> = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)? as u64,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?.collect::<Result<Vec<_>, _>>()?;

        let mut scenes = Vec::new();
        for (id, name, kind, global_json, launchpad_btn, launchpad_is_cc, launchpad_color) in scene_rows {
            // Load scene masks
            let mut stmt = self.conn.prepare(
                "SELECT mask_id, mask_type, x, y, params_json FROM scene_masks WHERE scene_id = ?1 ORDER BY display_order"
            )?;
            let scene_masks = stmt.query_map([id as i64], |row| {
                let params_json: String = row.get(4)?;
                let params: HashMap<String, serde_json::Value> = serde_json::from_str(&params_json)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

                Ok(Mask {
                    id: row.get::<_, i64>(0)? as u64,
                    mask_type: row.get(1)?,
                    x: row.get(2)?,
                    y: row.get(3)?,
                    params,
                })
            })?.collect::<Result<Vec<_>, _>>()?;

            let global = global_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .context("Failed to parse global effect JSON")?;

            scenes.push(Scene {
                id,
                name,
                kind,
                masks: scene_masks,
                global,
                launchpad_btn: launchpad_btn.map(|v| v as u8),
                launchpad_is_cc: launchpad_is_cc != 0,
                launchpad_color: launchpad_color.map(|v| v as u8),
            });
        }

        // Load app config
        let (
            selected_scene_id,
            network_use_multicast,
            network_unicast_ip,
            network_universe,
            bind_address,
            mode,
            effect,
            audio_latency_ms,
            audio_use_flywheel,
            audio_hybrid_sync,
            audio_sensitivity,
            layout_locked,
        ) = self.conn.query_row(
            "SELECT selected_scene_id, network_use_multicast, network_unicast_ip, network_universe,
                    bind_address, mode, effect, audio_latency_ms, audio_use_flywheel,
                    audio_hybrid_sync, audio_sensitivity, layout_locked
             FROM app_config WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<u64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u16>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, f32>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, f32>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            }
        )?;

        Ok(AppState {
            strips,
            masks,
            scenes,
            selected_scene_id,
            network: NetworkConfig {
                use_multicast: network_use_multicast != 0,
                unicast_ip: network_unicast_ip,
                universe: network_universe,
            },
            audio: AudioConfig {
                latency_ms: audio_latency_ms,
                use_flywheel: audio_use_flywheel != 0,
                hybrid_sync: audio_hybrid_sync != 0,
                sensitivity: audio_sensitivity,
            },
            bind_address,
            mode,
            effect,
            layout_locked: layout_locked != 0,
        })
    }

    /// Save entire app state to database (transactional)
    pub fn save_state(&mut self, state: &AppState) -> Result<()> {
        let tx = self.conn.transaction()?;

        // Clear and re-insert all data (simpler than diffing for updates)
        tx.execute("DELETE FROM scene_masks", [])?;
        tx.execute("DELETE FROM scenes", [])?;
        tx.execute("DELETE FROM masks", [])?;
        tx.execute("DELETE FROM strips", [])?;

        // Save strips
        for strip in &state.strips {
            tx.execute(
                "INSERT INTO strips (id, universe, start_channel, pixel_count, x, y, spacing, flipped, color_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    strip.id as i64,
                    strip.universe,
                    strip.start_channel,
                    strip.pixel_count,
                    strip.x,
                    strip.y,
                    strip.spacing,
                    if strip.flipped { 1 } else { 0 },
                    strip.color_order,
                ],
            )?;
        }

        // Save global masks
        for mask in &state.masks {
            let params_json = serde_json::to_string(&mask.params)?;
            tx.execute(
                "INSERT INTO masks (id, mask_type, x, y, params_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![mask.id as i64, mask.mask_type, mask.x, mask.y, params_json],
            )?;
        }

        // Save scenes
        for scene in &state.scenes {
            let global_effect_json = scene.global.as_ref()
                .map(|g| serde_json::to_string(g))
                .transpose()?;

            tx.execute(
                "INSERT INTO scenes (id, name, kind, global_effect_json, launchpad_btn, launchpad_is_cc, launchpad_color)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    scene.id as i64,
                    scene.name,
                    scene.kind,
                    global_effect_json,
                    scene.launchpad_btn.map(|v| v as i64),
                    if scene.launchpad_is_cc { 1 } else { 0 },
                    scene.launchpad_color.map(|v| v as i64),
                ],
            )?;

            // Save scene masks
            for (idx, mask) in scene.masks.iter().enumerate() {
                let params_json = serde_json::to_string(&mask.params)?;
                tx.execute(
                    "INSERT INTO scene_masks (scene_id, mask_id, mask_type, x, y, params_json, display_order)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        scene.id as i64,
                        mask.id as i64,
                        mask.mask_type,
                        mask.x,
                        mask.y,
                        params_json,
                        idx as i64,
                    ],
                )?;
            }
        }

        // Save app config
        tx.execute(
            "UPDATE app_config SET
                selected_scene_id = ?1,
                network_use_multicast = ?2,
                network_unicast_ip = ?3,
                network_universe = ?4,
                bind_address = ?5,
                mode = ?6,
                effect = ?7,
                audio_latency_ms = ?8,
                audio_use_flywheel = ?9,
                audio_hybrid_sync = ?10,
                audio_sensitivity = ?11,
                layout_locked = ?12
             WHERE id = 1",
            params![
                state.selected_scene_id,
                if state.network.use_multicast { 1 } else { 0 },
                state.network.unicast_ip,
                state.network.universe,
                state.bind_address,
                state.mode,
                state.effect,
                state.audio.latency_ms,
                if state.audio.use_flywheel { 1 } else { 0 },
                if state.audio.hybrid_sync { 1 } else { 0 },
                state.audio.sensitivity,
                if state.layout_locked { 1 } else { 0 },
            ],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Export entire state to JSON string
    pub fn export_to_json(&self) -> Result<String> {
        let state = self.load_state()?;
        let json = serde_json::to_string_pretty(&state)?;
        Ok(json)
    }

    /// Import from JSON string
    pub fn import_from_json(&mut self, json: &str, merge: bool) -> Result<()> {
        let import_state: AppState = serde_json::from_str(json)
            .context("Invalid JSON format")?;

        let tx = self.conn.transaction()?;

        if !merge {
            // Replace mode: clear all existing data
            tx.execute("DELETE FROM scene_masks", [])?;
            tx.execute("DELETE FROM scenes", [])?;
            tx.execute("DELETE FROM masks", [])?;
            tx.execute("DELETE FROM strips", [])?;
        }

        // Import strips (handle ID conflicts in merge mode)
        for strip in &import_state.strips {
            if merge {
                // In merge mode, find max ID and offset if needed
                let exists: bool = tx.query_row(
                    "SELECT COUNT(*) > 0 FROM strips WHERE id = ?1",
                    [strip.id],
                    |row| row.get(0)
                )?;

                if exists {
                    // Skip or generate new ID
                    continue;
                }
            }

            tx.execute(
                "INSERT INTO strips (id, universe, start_channel, pixel_count, x, y, spacing, flipped, color_order)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    strip.id as i64,
                    strip.universe,
                    strip.start_channel,
                    strip.pixel_count,
                    strip.x,
                    strip.y,
                    strip.spacing,
                    if strip.flipped { 1 } else { 0 },
                    strip.color_order,
                ],
            )?;
        }

        // Import scenes and masks similarly
        for scene in &import_state.scenes {
            if merge {
                let exists: bool = tx.query_row(
                    "SELECT COUNT(*) > 0 FROM scenes WHERE id = ?1",
                    [scene.id],
                    |row| row.get(0)
                )?;
                if exists {
                    continue;
                }
            }

            let global_effect_json = scene.global.as_ref()
                .map(|g| serde_json::to_string(g))
                .transpose()?;

            tx.execute(
                "INSERT INTO scenes (id, name, kind, global_effect_json, launchpad_btn, launchpad_is_cc, launchpad_color)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    scene.id as i64,
                    scene.name,
                    scene.kind,
                    global_effect_json,
                    scene.launchpad_btn.map(|v| v as i64),
                    if scene.launchpad_is_cc { 1 } else { 0 },
                    scene.launchpad_color.map(|v| v as i64),
                ],
            )?;

            for (idx, mask) in scene.masks.iter().enumerate() {
                let params_json = serde_json::to_string(&mask.params)?;
                tx.execute(
                    "INSERT INTO scene_masks (scene_id, mask_id, mask_type, x, y, params_json, display_order)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        scene.id as i64,
                        mask.id as i64,
                        mask.mask_type,
                        mask.x,
                        mask.y,
                        params_json,
                        idx as i64,
                    ],
                )?;
            }
        }

        // In replace mode, update app config
        if !merge {
            tx.execute(
                "UPDATE app_config SET
                    selected_scene_id = ?1,
                    network_use_multicast = ?2,
                    network_unicast_ip = ?3,
                    network_universe = ?4,
                    audio_latency_ms = ?5,
                    audio_use_flywheel = ?6,
                    audio_hybrid_sync = ?7,
                    audio_sensitivity = ?8,
                    layout_locked = ?9
                 WHERE id = 1",
                params![
                    import_state.selected_scene_id,
                    if import_state.network.use_multicast { 1 } else { 0 },
                    import_state.network.unicast_ip,
                    import_state.network.universe,
                    import_state.audio.latency_ms,
                    if import_state.audio.use_flywheel { 1 } else { 0 },
                    if import_state.audio.hybrid_sync { 1 } else { 0 },
                    import_state.audio.sensitivity,
                    if import_state.layout_locked { 1 } else { 0 },
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }
}
