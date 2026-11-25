// Dragon Radar Tool - Motor Visual de Radar Táctico (Canvas + HUD)

const canvas = document.getElementById('radarCanvas');
const ctx = canvas.getContext('2d');
const searchInput = document.getElementById('searchInput');
const resultsContainer = document.getElementById('resultsContainer');
const blipCountEl = document.getElementById('blipCount');
const hudClock = document.getElementById('hudClock');
const quickLookModal = document.getElementById('quickLookModal');
const modalTitle = document.getElementById('modalTitle');
const modalCodePreview = document.getElementById('modalCodePreview');
const modalCloseBtn = document.getElementById('modalCloseBtn');

// Estado del Radar
let sweepAngle = 0;
const sweepSpeed = 0.04;
let radarBlips = [];
let selectedIndex = -1;
let currentResults = [];
let currentMode = 'all';

// Cargar estado real de Dragon Radar desde el servidor (incluye progreso de checkpoints/validación)
async function updateHudStatus() {
    try {
        const res = await fetch('/api/status');
        if (res.ok) {
            const data = await res.json();
            const dbStats = document.getElementById('dbStats');
            const statusInd = document.getElementById('statusIndicator');
            if (dbStats) {
                dbStats.textContent = `REDB: ${data.redb_records.toLocaleString()} | AI: ${data.vector_count} VECTORS`;
            }
            if (statusInd) {
                if (data.phase === 'INITIAL_INDEXING') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> INDEXACIÓN INICIAL [${data.target_index}/${data.total_targets}] (+${data.new_files})`;
                } else if (data.phase === 'RESUMING_CHECKPOINT') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> REANUDANDO CHECKPOINT [${data.target_index}/${data.total_targets}]`;
                } else if (data.phase === 'VALIDATING_INDEX') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> VALIDANDO ÍNDICE [${data.target_index}/${data.total_targets}]`;
                } else {
                    statusInd.innerHTML = `<span class="blink-dot"></span> MFT ENGINE: ARMED`;
                }
            }
        }
    } catch (_) {}
}
updateHudStatus();
setInterval(updateHudStatus, 2000);

// Botones de Modo
document.querySelectorAll('.mode-btn').forEach(btn => {
    btn.addEventListener('click', () => {
        document.querySelectorAll('.mode-btn').forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        currentMode = btn.dataset.mode;
        performSearch(searchInput.value);
    });
});

// Reloj HUD
setInterval(() => {
    const now = new Date();
    hudClock.textContent = now.toTimeString().split(' ')[0] + '.' + String(now.getMilliseconds()).padStart(3, '0').slice(0, 2);
}, 80);

// Base de datos de demostración conectada con las crates de Dragon Radar Tool
const mockDataset = [
    { name: 'ntfs.rs', path: 'crates/dragon-mft/src/ntfs.rs', badge: 'RUST', size: '10.2 KB', score: 2.58, type: 'code', snippet: 'DeviceIoControl(handle, FSCTL_ENUM_USN_DATA, ...);' },
    { name: 'fallback.rs', path: 'crates/dragon-mft/src/fallback.rs', badge: 'RUST', size: '3.4 KB', score: 2.10, type: 'code', snippet: 'jwalk::WalkDir::new(&root_path).skip_hidden(false)...' },
    { name: 'lib.rs', path: 'crates/dragon-mft/src/lib.rs', badge: 'RUST', size: '4.1 KB', score: 1.84, type: 'code', snippet: 'pub struct UnifiedScanner; scan_volume_mft(volume)...' },
    { name: 'Cargo.toml', path: 'Cargo.toml', badge: 'TOML', size: '1.2 KB', score: 1.65, type: 'config', snippet: '[workspace] members = ["crates/dragon-core", "dragon-mft"...]' },
    { name: 'main.rs', path: 'crates/dragon-cli/src/main.rs', badge: 'CLI', size: '5.8 KB', score: 1.45, type: 'code', snippet: 'Commands::Scan { target, limit } => UnifiedScanner::scan(&target)...' },
    { name: 'lib.rs', path: 'crates/dragon-storage/src/lib.rs', badge: 'STORAGE', size: '3.8 KB', score: 1.30, type: 'db', snippet: 'pub struct DragonStorage { db: redb::Database }...' },
    { name: 'lib.rs', path: 'crates/dragon-index/src/lib.rs', badge: 'INDEX', size: '4.9 KB', score: 1.15, type: 'search', snippet: 'pub struct DragonIndexEngine { index: TantivyIndex }...' },
    { name: 'main.rs', path: 'crates/dragon-service/src/main.rs', badge: 'SERVICE', size: '3.1 KB', score: 1.05, type: 'ipc', snippet: 'DEFAULT_PIPE_NAME = r"\\\\.\\pipe\\dragon_radar_mft"...' },
    { name: 'dragon_radar_architecture.md', path: 'docs/dragon_radar_architecture.md', badge: 'DOC', size: '24.5 KB', score: 0.95, type: 'doc', snippet: '# Dragon Radar Tool: Arquitectura Técnica y Plan de Construcción...' },
    { name: 'dragon.exe', path: 'target/release/dragon.exe', badge: 'BIN', size: '18.4 MB', score: 0.85, type: 'binary', snippet: '[Binario Nativo Win32/x64 compilado con optimización L3]' }
];

// Generar coordenadas polares aleatorias consistentes basadas en el nombre
function generateBlipCoordinates(item, index, total) {
    let hash = 0;
    for (let i = 0; i < item.name.length; i++) {
        hash = (hash << 5) - hash + item.name.charCodeAt(i);
        hash |= 0;
    }
    const angle = Math.abs(hash % 360) * (Math.PI / 180);
    // Radio entre 0.2 y 0.88 del centro
    const radiusNorm = 0.2 + (Math.abs((hash >> 3) % 68) / 100);
    return {
        angle,
        radiusNorm,
        item,
        index,
        pingIntensity: 1.0
    };
}

// Búsqueda y filtrado reactivo conectado a la API de Dragon Radar
async function performSearch(query) {
    const q = query.trim();
    let results = [];

    if (q) {
        try {
            const res = await fetch(`/api/search?q=${encodeURIComponent(q)}&mode=${currentMode}&limit=25`);
            if (res.ok) {
                const apiResults = await res.json();
                if (Array.isArray(apiResults) && apiResults.length > 0) {
                    results = apiResults;
                }
            }
        } catch (_) {}
    }

    // Si la API no retorna resultados o estamos en modo estático, fallback local
    if (results.length === 0) {
        const lower = q.toLowerCase();
        if (!lower) {
            results = mockDataset.slice(0, 6);
        } else {
            results = mockDataset.filter(item => 
                item.name.toLowerCase().includes(lower) ||
                item.path.toLowerCase().includes(lower) ||
                item.snippet.toLowerCase().includes(lower)
            );
        }
    }

    currentResults = results;

    // Actualizar blips en el radar
    radarBlips = results.map((item, idx) => generateBlipCoordinates(item, idx, results.length));
    blipCountEl.textContent = results.length;
    selectedIndex = results.length > 0 ? 0 : -1;

    renderResults(results);
}

function renderResults(results) {
    if (results.length === 0) {
        resultsContainer.innerHTML = `
            <div class="empty-state">
                <p class="empty-title">SIN SEÑAL EN ESTA FRECUENCIA</p>
                <p class="empty-subtitle">No se detectaron coincidencias para el término buscado</p>
            </div>
        `;
        return;
    }

    resultsContainer.innerHTML = results.map((item, idx) => `
        <div class="result-row ${idx === selectedIndex ? 'selected' : ''}" data-index="${idx}">
            <div class="result-badge ${item.badge === 'RUST' || item.badge === 'SERVICE' ? '' : 'badge-amber'}">${item.badge}</div>
            <div class="result-details">
                <div class="result-filename">${item.name}</div>
                <div class="result-path">${item.path}</div>
            </div>
            <div class="result-meta">
                <div>${item.size}</div>
                <div style="font-size: 9px; color: #5c8c67;">Score: ${item.score}</div>
            </div>
        </div>
    `).join('');

    // Listeners de clics
    document.querySelectorAll('.result-row').forEach(row => {
        row.addEventListener('click', () => {
            const idx = parseInt(row.dataset.index);
            selectResult(idx);
            openQuickLook(results[idx]);
        });
        row.addEventListener('mouseenter', () => {
            const idx = parseInt(row.dataset.index);
            selectResult(idx);
        });
    });
}

function selectResult(index) {
    selectedIndex = index;
    document.querySelectorAll('.result-row').forEach((row, idx) => {
        if (idx === index) {
            row.classList.add('selected');
            row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
        } else {
            row.classList.remove('selected');
        }
    });
}

async function openQuickLook(item) {
    modalTitle.textContent = `VISTA RÁPIDA: ${item.path}`;
    modalCodePreview.textContent = `// Conectando con el motor de extracción Dragon Radar...\n// Archivo: ${item.path}`;
    quickLookModal.classList.add('open');

    try {
        const res = await fetch(`/api/extract?file=${encodeURIComponent(item.path)}`);
        if (res.ok) {
            const data = await res.json();
            if (data.success && data.content) {
                modalCodePreview.textContent = `// Formato detectado: ${data.format} | ${data.content.length} caracteres extraídos\n\n${data.content}`;
                return;
            }
        }
    } catch (_) {}

    modalCodePreview.textContent = `// Ruta del archivo: ${item.path}
// Tamaño: ${item.size} | Relevancia: ${item.score}

${item.snippet}

[Contenido indexado en motor Dragon Radar (Tantivy / Redb / Model2Vec)]`;
}

modalCloseBtn.addEventListener('click', () => quickLookModal.classList.remove('open'));
quickLookModal.addEventListener('click', (e) => {
    if (e.target === quickLookModal) quickLookModal.classList.remove('open');
});

// Renderizado del Radar Canvas (Canvas 2D)
function drawRadar() {
    const width = canvas.width;
    const height = canvas.height;
    const cx = width / 2;
    const cy = height / 2;
    const maxRadius = width / 2 - 8;

    // Limpiar con leve desvanecimiento para estela de radar
    ctx.fillStyle = 'rgba(4, 12, 6, 0.25)';
    ctx.fillRect(0, 0, width, height);

    // Círculos concéntricos de alcance
    ctx.strokeStyle = 'rgba(0, 255, 102, 0.25)';
    ctx.lineWidth = 1;
    for (let r = 0.25; r <= 1.0; r += 0.25) {
        ctx.beginPath();
        ctx.arc(cx, cy, maxRadius * r, 0, Math.PI * 2);
        ctx.stroke();
    }

    // Dibujar línea de barrido rotatorio
    sweepAngle += sweepSpeed;
    if (sweepAngle >= Math.PI * 2) sweepAngle -= Math.PI * 2;

    const sweepX = cx + Math.cos(sweepAngle) * maxRadius;
    const sweepY = cy + Math.sin(sweepAngle) * maxRadius;

    const gradient = ctx.createRadialGradient(cx, cy, 0, cx, cy, maxRadius);
    gradient.addColorStop(0, 'rgba(0, 255, 102, 0.3)');
    gradient.addColorStop(1, 'rgba(0, 255, 102, 0.0)');

    ctx.save();
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.arc(cx, cy, maxRadius, sweepAngle - 0.4, sweepAngle);
    ctx.closePath();
    ctx.fillStyle = 'rgba(0, 255, 102, 0.15)';
    ctx.fill();

    // Línea frontal de barrido
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.lineTo(sweepX, sweepY);
    ctx.strokeStyle = '#00ff66';
    ctx.lineWidth = 2;
    ctx.shadowColor = '#00ff66';
    ctx.shadowBlur = 8;
    ctx.stroke();
    ctx.restore();

    // Dibujar Blips (Dragon Balls / Archivos detectados)
    radarBlips.forEach((blip, idx) => {
        const blipRadius = blip.radiusNorm * maxRadius;
        const bx = cx + Math.cos(blip.angle) * blipRadius;
        const by = cy + Math.sin(blip.angle) * blipRadius;

        // Si la aguja de barrido pasa por el blip, intensificar el pulso
        const angleDiff = Math.abs(sweepAngle - blip.angle);
        if (angleDiff < 0.1 || angleDiff > Math.PI * 2 - 0.1) {
            blip.pingIntensity = 1.0;
        } else {
            blip.pingIntensity = Math.max(0.2, blip.pingIntensity - 0.008);
        }

        const isSelected = idx === selectedIndex;
        ctx.save();

        if (isSelected) {
            // Anillo de enfoque táctico alrededor del blip seleccionado
            ctx.beginPath();
            ctx.arc(bx, by, 12, 0, Math.PI * 2);
            ctx.strokeStyle = '#ffaa00';
            ctx.lineWidth = 1.5;
            ctx.setLineDash([3, 3]);
            ctx.stroke();
            ctx.setLineDash([]);
        }

        // Blip principal (Dragon Ball ámbar brillante)
        ctx.beginPath();
        ctx.arc(bx, by, isSelected ? 6 : 4, 0, Math.PI * 2);
        ctx.fillStyle = isSelected 
            ? '#ffcc00' 
            : `rgba(255, 170, 0, ${blip.pingIntensity})`;
        ctx.shadowColor = '#ffaa00';
        ctx.shadowBlur = isSelected ? 12 : 6;
        ctx.fill();

        ctx.restore();
    });

    requestAnimationFrame(drawRadar);
}

// Navegación por Teclado
window.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') {
        e.preventDefault();
        if (radarBlips.length > 0) {
            selectedIndex = (selectedIndex + 1) % radarBlips.length;
            selectResult(selectedIndex);
        }
    } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (radarBlips.length > 0) {
            selectedIndex = (selectedIndex - 1 + radarBlips.length) % radarBlips.length;
            selectResult(selectedIndex);
        }
    } else if (e.key === ' ') {
        if (document.activeElement !== searchInput && selectedIndex >= 0 && selectedIndex < radarBlips.length) {
            e.preventDefault();
            openQuickLook(radarBlips[selectedIndex].item);
        }
    } else if (e.key === 'Enter') {
        if (selectedIndex >= 0 && selectedIndex < radarBlips.length) {
            const item = radarBlips[selectedIndex].item;
            fetch(`/api/open?file=${encodeURIComponent(item.path)}`).catch(() => {});
        }
    } else if (e.key === 'Escape') {
        quickLookModal.classList.remove('open');
    }
});

// Listener de Búsqueda
searchInput.addEventListener('input', (e) => performSearch(e.target.value));

// Iniciar radar y búsqueda inicial
performSearch('');
drawRadar();
