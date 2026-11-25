// Dragon Radar Tool - Motor Visual Anime Estilo Akira Toriyama (Basado en radar modelo.webp)

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
const radarTopBtn = document.getElementById('radarTopBtn');
const sweepStatusEl = document.getElementById('sweepStatus');
const lanAgentBanner = document.getElementById('lanAgentBanner');
const lanAgentDownloadBtn = document.getElementById('lanAgentDownloadBtn');

// Resolución de motor local:
// Si la interfaz web se abre desde otro servidor de la red LAN, consulta el motor local del propio equipo (127.0.0.1)
// para buscar e indexar exclusivamente los archivos locales de este equipo y nunca los del servidor.
const isLocalOrigin = ['127.0.0.1', 'localhost', '::1', ''].includes(window.location.hostname);
const LOCAL_AGENT_BASE = `http://127.0.0.1:${window.location.port || '8088'}`;
let apiBase = isLocalOrigin ? '' : LOCAL_AGENT_BASE;
let localAgentOnline = isLocalOrigin;

if (lanAgentDownloadBtn && !isLocalOrigin) {
    lanAgentDownloadBtn.href = `${window.location.origin}/api/agent/launcher`;
}

// Estado del Radar Anime
let pulsePhase = 0;
let pingWaveRadius = 0; // Onda de ping al pulsar el botón superior
let pingActive = false;
let zoomLevel = 1; // 1 = cuadrícula clásica 8x8, 2 = zoom 12x12, 3 = zoom 5x5
let radarBlips = [];
let selectedIndex = -1;
let currentResults = [];
let currentMode = 'all';

// Cargar estado real de Dragon Radar desde el motor del equipo local
async function updateHudStatus() {
    try {
        const res = await fetch(`${apiBase}/api/status`);
        if (res.ok) {
            const data = await res.json();
            localAgentOnline = true;
            if (lanAgentBanner) lanAgentBanner.hidden = true;

            const dbStats = document.getElementById('dbStats');
            const statusInd = document.getElementById('statusIndicator');
            const machineLabel = data.machine_name ? `${data.machine_name} | ` : '';
            if (dbStats) {
                dbStats.textContent = `${machineLabel}ARCHIVOS: ${data.redb_records.toLocaleString()} | CONTENIDO: ${data.vector_count}`;
            }
            if (statusInd) {
                if (data.phase === 'INITIAL_INDEXING') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> INDEXANDO [${data.target_index}/${data.total_targets}] (+${data.new_files})`;
                } else if (data.phase === 'RESUMING_CHECKPOINT') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> REANUDANDO PUNTO DE CONTROL [${data.target_index}/${data.total_targets}]`;
                } else if (data.phase === 'VALIDATING_INDEX') {
                    statusInd.innerHTML = `<span class="blink-dot"></span> VALIDANDO EQUIPO [${data.target_index}/${data.total_targets}]`;
                } else {
                    statusInd.innerHTML = `<span class="blink-dot"></span> RADAR LOCAL ACTIVO`;
                }
            }
            return;
        }
    } catch (_) {}

    if (!isLocalOrigin) {
        localAgentOnline = false;
        if (lanAgentBanner) lanAgentBanner.hidden = false;
        const statusInd = document.getElementById('statusIndicator');
        if (statusInd) {
            statusInd.innerHTML = `<span class="blink-dot"></span> ESPERANDO MOTOR LOCAL`;
        }
    }
}
updateHudStatus();
setInterval(updateHudStatus, 2000);

// Botón superior del Radar (estilo pulsador Toriyama: cambia zoom de cuadrícula y dispara ping)
if (radarTopBtn) {
    radarTopBtn.addEventListener('click', () => {
        zoomLevel = (zoomLevel % 3) + 1;
        pingWaveRadius = 8;
        pingActive = true;
        if (sweepStatusEl) {
            sweepStatusEl.textContent = zoomLevel === 1 ? 'ACTIVO (1X)' : zoomLevel === 2 ? 'AMPLIADO (2X)' : 'DETALLE (3X)';
        }
    });
}

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
    hudClock.textContent = now.toTimeString().split(' ')[0];
}, 500);

// Dataset demo cuando se previsualiza sin servidor HTTP activo
const mockDataset = [
    { name: 'contrato_2026.pdf', path: 'Documents/contrato_2026.pdf', badge: 'PDF', size: '420 KB', score: '980.0', snippet: 'Documento PDF de ejemplo indexado por nombre y contenido.' },
    { name: 'logo_capsule.svg', path: 'Design/logo_capsule.svg', badge: 'VECTOR', size: '34 KB', score: '710.0', snippet: 'Archivo vectorial indexado por nombre únicamente.' },
    { name: 'dragon_radar.rs', path: 'crates/dragon-cli/src/main.rs', badge: 'RUST', size: '14 KB', score: '640.0', snippet: 'Código fuente principal del radar híbrido.' },
    { name: 'intro_anime.mp4', path: 'Videos/intro_anime.mp4', badge: 'VIDEO', size: '48.2 MB', score: '520.0', snippet: 'Archivo multimedia indexado por nombre únicamente.' }
];

// Generar coordenadas consistentes estilo Esferas del Dragón en la cuadrícula
function generateBlipCoordinates(item, index) {
    const str = item.path || item.name;
    let hash = 2166136261;
    for (let i = 0; i < str.length; i++) {
        hash ^= str.charCodeAt(i);
        hash = Math.imul(hash, 16777619);
    }
    const angle = (Math.abs(hash % 360)) * (Math.PI / 180);
    // Las primeras coincidencias (mayor relevancia) aparecen más cerca del triángulo central
    const tierRadius = 0.22 + Math.min(index, 12) * 0.045;
    const jitter = (Math.abs((hash >> 5) % 14)) / 100;
    const radiusNorm = Math.min(0.84, tierRadius + jitter);
    return {
        angle,
        radiusNorm,
        item,
        index,
        bx: 0,
        by: 0
    };
}

let searchTimer = null;
async function performSearch(query) {
    const q = query.trim();
    let results = [];
    let usedApi = false;

    if (!isLocalOrigin && !localAgentOnline) {
        currentResults = [];
        radarBlips = [];
        blipCountEl.textContent = '0';
        selectedIndex = -1;
        resultsContainer.innerHTML = `
            <div class="empty-state">
                <p class="empty-title">ACTIVA EL RADAR EN ESTE EQUIPO</p>
                <p class="empty-subtitle">Haz clic en "ACTIVAR EN ESTE EQUIPO" arriba para escanear y buscar los archivos locales de esta computadora.</p>
            </div>
        `;
        return;
    }

    if (q) {
        try {
            const res = await fetch(`${apiBase}/api/search?q=${encodeURIComponent(q)}&mode=${currentMode}&limit=25`);
            if (res.ok) {
                const apiResults = await res.json();
                if (Array.isArray(apiResults)) {
                    results = apiResults;
                    usedApi = true;
                }
            }
        } catch (_) {}
    }

    if (!usedApi && isLocalOrigin && window.location.protocol === 'file:' && (!q || results.length === 0)) {
        const lower = q.toLowerCase();
        if (!lower) {
            results = mockDataset;
        } else {
            results = mockDataset.filter(item =>
                item.name.toLowerCase().includes(lower) ||
                item.path.toLowerCase().includes(lower) ||
                item.snippet.toLowerCase().includes(lower)
            );
        }
    }

    currentResults = results;
    radarBlips = results.map((item, idx) => generateBlipCoordinates(item, idx));
    blipCountEl.textContent = results.length;
    selectedIndex = results.length > 0 ? 0 : -1;

    pingWaveRadius = 10;
    pingActive = results.length > 0;

    renderResults(results);
}

function getBadgeClass(badge) {
    if (['PDF', 'WORD', 'EXCEL', 'PPTX', 'EBOOK', 'DOC', 'RUST', 'JS', 'CODE'].includes(badge)) return '';
    if (['VECTOR', 'DESIGN', 'CAD/3D', 'IMG'].includes(badge)) return 'badge-amber';
    if (['VIDEO', 'AUDIO', 'MEDIA'].includes(badge)) return 'badge-sky';
    return 'badge-gray';
}

function escapeHtml(str) {
    return String(str || '')
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#39;');
}

function renderResults(results) {
    if (results.length === 0) {
        resultsContainer.innerHTML = `
            <div class="empty-state">
                <p class="empty-title">SIN SEÑAL EN EL RADAR</p>
                <p class="empty-subtitle">No se detectaron archivos para esta consulta</p>
            </div>
        `;
        return;
    }

    resultsContainer.innerHTML = results.map((item, idx) => `
        <div class="result-row ${idx === selectedIndex ? 'selected' : ''}" data-index="${idx}">
            <div class="result-badge ${getBadgeClass(item.badge)}">${escapeHtml(item.badge)}</div>
            <div class="result-details">
                <div class="result-filename">${escapeHtml(item.name)}</div>
                <div class="result-path">${escapeHtml(item.path)}</div>
                ${item.snippet ? `<div class="result-snippet">${escapeHtml(item.snippet)}</div>` : ''}
            </div>
            <div class="result-meta">
                <div>${escapeHtml(item.size)}</div>
                <div class="result-score-sub">N.${idx + 1} · ${escapeHtml(item.score)}</div>
            </div>
        </div>
    `).join('');

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
    modalCodePreview.textContent = `// Extrayendo vista previa de ${item.path}...`;
    quickLookModal.classList.add('open');

    try {
        const res = await fetch(`${apiBase}/api/extract?file=${encodeURIComponent(item.path)}`);
        if (res.ok) {
            const data = await res.json();
            if (data.success && data.content) {
                modalCodePreview.textContent = `// Formato: ${data.format} | ${data.content.length} caracteres\n\n${data.content}`;
                return;
            }
        }
    } catch (_) {}

    modalCodePreview.textContent = `// Archivo: ${item.path}
// Tamaño: ${item.size} | Puntuación: ${item.score}

${item.snippet}`;
}

modalCloseBtn.addEventListener('click', () => quickLookModal.classList.remove('open'));
quickLookModal.addEventListener('click', (e) => {
    if (e.target === quickLookModal) quickLookModal.classList.remove('open');
});

// Clic directo sobre los blips del radar Canvas
canvas.addEventListener('click', (e) => {
    const rect = canvas.getBoundingClientRect();
    const scaleX = canvas.width / rect.width;
    const scaleY = canvas.height / rect.height;
    const clickX = (e.clientX - rect.left) * scaleX;
    const clickY = (e.clientY - rect.top) * scaleY;

    let bestIdx = -1;
    let bestDist = 22;
    radarBlips.forEach((b, idx) => {
        const d = Math.hypot(clickX - b.bx, clickY - b.by);
        if (d < bestDist) {
            bestDist = d;
            bestIdx = idx;
        }
    });
    if (bestIdx >= 0) {
        selectResult(bestIdx);
    }
});

// Dibujo Anime del Radar (Fiel a radar modelo.webp: fondo verde, cuadrícula cuadrada oscura, triángulo rojo central, blips luminosos con número)
function drawRadar() {
    const w = canvas.width;
    const h = canvas.height;
    const cx = w / 2;
    const cy = h / 2;
    const maxRadius = w / 2 - 4;

    pulsePhase += 0.06;

    // 1. Fondo verde esmeralda anime con ligero degradado cel-shading
    const bgGrad = ctx.createRadialGradient(cx - 40, cy - 45, 20, cx, cy, maxRadius);
    bgGrad.addColorStop(0, '#0E8E1E');
    bgGrad.addColorStop(0.7, '#086C14');
    bgGrad.addColorStop(1, '#05510D');
    ctx.fillStyle = bgGrad;
    ctx.fillRect(0, 0, w, h);

    // 2. Cuadrícula ortogonal clásica de Toriyama (como en radar modelo.webp)
    const cells = zoomLevel === 1 ? 8 : zoomLevel === 2 ? 12 : 6;
    const step = w / cells;
    ctx.save();
    ctx.strokeStyle = '#032908';
    ctx.lineWidth = 2.2;
    ctx.beginPath();
    for (let i = 1; i < cells; i++) {
        const pos = Math.round(i * step) + 0.5;
        // Línea vertical
        ctx.moveTo(pos, 0);
        ctx.lineTo(pos, h);
        // Línea horizontal
        ctx.moveTo(0, pos);
        ctx.lineTo(w, pos);
    }
    ctx.stroke();

    // Borde interno sutil del cristal del radar
    ctx.beginPath();
    ctx.arc(cx, cy, maxRadius - 2, 0, Math.PI * 2);
    ctx.strokeStyle = 'rgba(0, 20, 4, 0.55)';
    ctx.lineWidth = 4;
    ctx.stroke();
    ctx.restore();

    // 3. Onda expansiva anime cuando se pulsa el botón o hay búsqueda
    if (pingActive) {
        pingWaveRadius += 5.5;
        const alpha = Math.max(0, 1 - pingWaveRadius / maxRadius);
        ctx.save();
        ctx.beginPath();
        ctx.arc(cx, cy, pingWaveRadius, 0, Math.PI * 2);
        ctx.strokeStyle = `rgba(210, 255, 140, ${alpha * 0.75})`;
        ctx.lineWidth = 3;
        ctx.stroke();
        ctx.restore();
        if (pingWaveRadius >= maxRadius) {
            pingActive = false;
        }
    }

    // 4. Blips luminosos amarillos/blancos con número anime (ej. • 1, • 2, • 4)
    const blinkScale = 0.88 + 0.12 * Math.sin(pulsePhase * 1.4);

    radarBlips.slice(0, 18).forEach((blip, idx) => {
        const r = blip.radiusNorm * (maxRadius - 24);
        const bx = cx + Math.cos(blip.angle) * r;
        const by = cy + Math.sin(blip.angle) * r;
        blip.bx = bx;
        blip.by = by;

        const isSelected = idx === selectedIndex;
        const baseRadius = (isSelected ? 10.5 : 7.5) * blinkScale;

        ctx.save();

        // Anillo anime de mira alrededor del blip seleccionado
        if (isSelected) {
            ctx.beginPath();
            ctx.arc(bx, by, 17, 0, Math.PI * 2);
            ctx.strokeStyle = '#FFD028';
            ctx.lineWidth = 2.5;
            ctx.setLineDash([5, 4]);
            ctx.stroke();
            ctx.setLineDash([]);
        }

        // Halo luminoso amarillo característico del anime
        ctx.shadowColor = '#FFEE44';
        ctx.shadowBlur = isSelected ? 18 : 12;

        ctx.beginPath();
        ctx.arc(bx, by, baseRadius, 0, Math.PI * 2);
        ctx.fillStyle = isSelected ? '#FFFDF0' : '#FFFFB8';
        ctx.fill();

        // Contorno del blip
        ctx.shadowBlur = 0;
        ctx.lineWidth = 1.5;
        ctx.strokeStyle = '#E6B800';
        ctx.stroke();

        // Etiqueta numérica anime al lado superior derecho del punto (como el "4" en radar modelo.webp)
        if (idx < 9 || isSelected) {
            ctx.font = `700 ${isSelected ? 15 : 13}px 'Fredoka', sans-serif`;
            ctx.lineWidth = 3;
            ctx.strokeStyle = '#032908';
            const label = String(idx + 1);
            ctx.strokeText(label, bx + 11, by - 7);
            ctx.fillStyle = isSelected ? '#FFE44D' : '#FFF8A6';
            ctx.fillText(label, bx + 11, by - 7);
        }

        ctx.restore();
    });

    // 5. Triángulo rojo central apuntando hacia arriba (icono exacto del Radar del Dragón)
    ctx.save();
    ctx.translate(cx, cy);
    ctx.beginPath();
    ctx.moveTo(0, -13);
    ctx.lineTo(11, 9);
    ctx.lineTo(-11, 9);
    ctx.closePath();
    ctx.fillStyle = '#EE1B1B';
    ctx.fill();
    ctx.lineWidth = 2.2;
    ctx.strokeStyle = '#4A0404';
    ctx.lineJoin = 'round';
    ctx.stroke();
    ctx.restore();

    // 6. Reflejo sutil de cristal anime en la esquina superior izquierda
    ctx.save();
    ctx.beginPath();
    ctx.ellipse(cx - 65, cy - 75, 46, 16, -0.65, 0, Math.PI * 2);
    ctx.fillStyle = 'rgba(255, 255, 255, 0.09)';
    ctx.fill();
    ctx.restore();

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
            fetch(`${apiBase}/api/open?file=${encodeURIComponent(item.path)}`).catch(() => {});
        }
    } else if (e.key === 'Escape') {
        quickLookModal.classList.remove('open');
    }
});

// Listener de Búsqueda con debounce ligero (120ms)
searchInput.addEventListener('input', (e) => {
    const val = e.target.value;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => performSearch(val), 120);
});

// Iniciar radar y búsqueda inicial
performSearch('');
drawRadar();
