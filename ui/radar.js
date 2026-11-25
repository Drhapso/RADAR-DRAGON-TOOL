// Dragon Radar Tool - Motor Visual Anime Estilo Akira Toriyama (Basado en radar modelo.webp)

const canvas = document.getElementById('radarCanvas');
const ctx = canvas.getContext('2d');
const searchInput = document.getElementById('searchInput');
const resultsContainer = document.getElementById('resultsContainer');
const blipCountEl = document.getElementById('blipCount');
const hudClock = document.getElementById('hudClock');
const quickLookModal = document.getElementById('quickLookModal');
const modalTitle = document.getElementById('modalTitle');
const modalSubtitle = document.getElementById('modalSubtitle');
const modalPreviewContainer = document.getElementById('modalPreviewContainer');
const modalOpenLocationBtn = document.getElementById('modalOpenLocationBtn');
const modalCloseBtn = document.getElementById('modalCloseBtn');
const radarTopBtn = document.getElementById('radarTopBtn');
const sweepStatusEl = document.getElementById('sweepStatus');
const lanAgentBanner = document.getElementById('lanAgentBanner');
const lanAgentDownloadBtn = document.getElementById('lanAgentDownloadBtn');
const libreofficeNoticeBanner = document.getElementById('libreofficeNoticeBanner');
const libreofficeNoticeCloseBtn = document.getElementById('libreofficeNoticeCloseBtn');
const extensionChipsBar = document.getElementById('extensionChipsBar');
const extensionChipsList = document.getElementById('extensionChipsList');

// Elementos del panel integrado de Vista Previa
const previewBadge = document.getElementById('previewBadge');
const previewTitle = document.getElementById('previewTitle');
const previewMeta = document.getElementById('previewMeta');
const previewPaneBody = document.getElementById('previewPaneBody');
const previewOpenLocationBtn = document.getElementById('previewOpenLocationBtn');
const previewCopyBtn = document.getElementById('previewCopyBtn');
const previewExpandBtn = document.getElementById('previewExpandBtn');

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
let activePreviewItem = null;
let activePreviewData = null;
let previewRequestSeq = 0;
let libreofficeNoticeDismissed = false;
let extChipsSeq = 0;

function extractExtHintPrefix(query) {
    const trimmed = String(query || '').trim().toLowerCase();
    if (!trimmed) return '';
    const tokens = trimmed.split(/\s+/);
    for (const tok of tokens) {
        if (tok.startsWith('ext:')) return tok.slice(4).replace(/^[*\.]+/, '');
        if (tok.startsWith('extension:')) return tok.slice(10).replace(/^[*\.]+/, '');
        if (tok.startsWith('tipo:')) return tok.slice(5).replace(/^[*\.]+/, '');
        if (tok.startsWith('*.') || (tok.startsWith('.') && tok.length > 1)) {
            return tok.replace(/^[*\.]+/, '');
        }
    }
    if (currentMode === 'extension') {
        return tokens[0].replace(/^[*\.]+/, '');
    }
    return '';
}

function shouldShowExtensionBar(query) {
    if (currentMode === 'extension') return true;
    const trimmed = String(query || '').trim().toLowerCase();
    return /(?:^|\s)(?:\*\.|\.[a-z0-9]{1,}|ext:|extension:|tipo:)/i.test(trimmed);
}

function updateSearchPlaceholder() {
    if (!searchInput) return;
    if (currentMode === 'extension') {
        searchInput.placeholder = 'ESCRIBE UNA EXTENSIÓN (EJ: PDF, .XLSX, *.PPTX O PDF CONTRATO)...';
    } else {
        searchInput.placeholder = 'BUSCAR ARCHIVOS, TEXTO O EXTENSIÓN (EJ: .PDF, *.XLSX, EXT:DOCX)...';
    }
}

function renderExtensionChips(extensions, activeQuery) {
    if (!extensionChipsBar || !extensionChipsList) return;
    const show = shouldShowExtensionBar(activeQuery) && Array.isArray(extensions) && extensions.length > 0;
    extensionChipsBar.hidden = !show;
    if (!show) return;

    const currentExtHint = extractExtHintPrefix(activeQuery);
    extensionChipsList.innerHTML = extensions.map(item => {
        const ext = String(item.extension || '').toLowerCase();
        const isActive = currentExtHint && currentExtHint === ext;
        return `<button type="button" class="ext-chip ${isActive ? 'active' : ''}" data-ext="${escapeHtml(ext)}" title="Buscar archivos .${escapeHtml(ext.toUpperCase())}">.${escapeHtml(ext.toUpperCase())}<span class="ext-chip-count">${escapeHtml(String(item.count || 0))}</span></button>`;
    }).join('');

    extensionChipsList.querySelectorAll('.ext-chip').forEach(btn => {
        btn.addEventListener('click', () => {
            const ext = btn.dataset.ext;
            if (!ext || !searchInput) return;
            searchInput.value = currentMode === 'extension' ? ext : `.${ext}`;
            searchInput.focus();
            performSearch(searchInput.value);
            refreshExtensionChips(searchInput.value);
        });
    });
}

async function refreshExtensionChips(query = '') {
    if (!extensionChipsBar || !extensionChipsList) return;
    if (!shouldShowExtensionBar(query)) {
        extensionChipsBar.hidden = true;
        return;
    }
    const seq = ++extChipsSeq;
    const prefix = extractExtHintPrefix(query);
    try {
        const url = prefix
            ? `${apiBase}/api/extensions?limit=18&q=${encodeURIComponent(prefix)}`
            : `${apiBase}/api/extensions?limit=18`;
        const res = await fetch(url);
        if (seq !== extChipsSeq || !res.ok) return;
        const data = await res.json();
        renderExtensionChips(data.extensions || [], query);
    } catch (_) {}
}

// Cargar estado real de Dragon Radar desde el motor del equipo local
async function updateHudStatus() {
    try {
        const res = await fetch(`${apiBase}/api/status`);
        if (res.ok) {
            const data = await res.json();
            localAgentOnline = true;
            if (lanAgentBanner) lanAgentBanner.hidden = true;
            if (libreofficeNoticeBanner) {
                const loInstalled = data.libreoffice_installed !== false;
                libreofficeNoticeBanner.hidden = loInstalled || libreofficeNoticeDismissed;
            }

            const dbStats = document.getElementById('dbStats');
            const statusInd = document.getElementById('statusIndicator');
            const machineLabel = data.machine_name ? `${data.machine_name} | ` : '';
            const editionTag = data.portable_edition ? 'PORTABLE' : 'RADAR LOCAL ACTIVO';
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
                    statusInd.innerHTML = `<span class="blink-dot"></span> ${editionTag}`;
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
        updateSearchPlaceholder();
        refreshExtensionChips(searchInput.value);
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
        } else if (currentMode === 'extension' || /^(?:\*\.|\.[a-z0-9]+|ext:|extension:|tipo:)/i.test(lower)) {
            const cleanExt = lower.replace(/^(?:ext:|extension:|tipo:)/i, '').replace(/^[*\.]+/, '');
            results = mockDataset.filter(item => {
                const dotIdx = item.name.lastIndexOf('.');
                const fileExt = dotIdx >= 0 ? item.name.slice(dotIdx + 1).toLowerCase() : '';
                return fileExt.startsWith(cleanExt);
            });
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
            const idx = parseInt(row.dataset.index, 10);
            selectResult(idx);
            loadPreview(results[idx]);
        });
        row.addEventListener('dblclick', (e) => {
            e.preventDefault();
            const idx = parseInt(row.dataset.index, 10);
            selectResult(idx);
            openFileLocation(results[idx], row);
        });
    });
}

function selectResult(index, shouldScroll = true) {
    selectedIndex = index;
    document.querySelectorAll('.result-row').forEach((row, idx) => {
        if (idx === index) {
            row.classList.add('selected');
            if (shouldScroll) {
                row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
            }
        } else {
            row.classList.remove('selected');
        }
    });
}

async function openFileLocation(item, rowEl = null) {
    if (!item || !item.path) return;
    if (rowEl) {
        rowEl.classList.add('opening-flash');
        setTimeout(() => rowEl.classList.remove('opening-flash'), 450);
    }
    if (previewMeta) {
        previewMeta.textContent = `📂 Abriendo ubicación: ${item.path}`;
    }
    try {
        await fetch(`${apiBase}/api/open?file=${encodeURIComponent(item.path)}`);
    } catch (_) {}
}

function updatePreviewActionButtons(item, data) {
    const hasItem = Boolean(item && item.path);
    const hasCopyableText = Boolean(data && typeof data.content === 'string' && data.content.trim().length > 0);

    if (previewOpenLocationBtn) previewOpenLocationBtn.disabled = !hasItem;
    if (previewExpandBtn) previewExpandBtn.disabled = !hasItem;
    if (previewCopyBtn) {
        previewCopyBtn.hidden = !hasCopyableText;
        previewCopyBtn.disabled = !hasCopyableText;
    }
    if (modalOpenLocationBtn) modalOpenLocationBtn.disabled = !hasItem;
}

async function fetchPreviewData(item) {
    try {
        const res = await fetch(`${apiBase}/api/preview?file=${encodeURIComponent(item.path)}`);
        if (res.ok) {
            const data = await res.json();
            if (data && data.success) {
                return data;
            }
        }
    } catch (_) {}

    // Fallback a /api/extract si el servidor es una instancia anterior
    try {
        const res = await fetch(`${apiBase}/api/extract?file=${encodeURIComponent(item.path)}`);
        if (res.ok) {
            const data = await res.json();
            if (data && data.success) {
                return {
                    success: true,
                    kind: 'code',
                    file: item.path,
                    name: item.name,
                    size_label: item.size,
                    content: data.content || ''
                };
            }
        }
    } catch (_) {}

    return {
        success: true,
        kind: 'fallback',
        file: item.path,
        name: item.name,
        size_label: item.size,
        content: item.snippet || 'Sin contenido textual extraíble para este archivo.'
    };
}

async function loadPreview(item) {
    if (!item) return;
    const reqId = ++previewRequestSeq;
    activePreviewItem = item;
    activePreviewData = null;

    if (previewBadge) previewBadge.textContent = item.badge || 'FILE';
    if (previewTitle) previewTitle.textContent = item.name || item.path;
    if (previewMeta) previewMeta.textContent = `Cargando vista previa · ${item.size || ''}...`;
    updatePreviewActionButtons(item, null);

    if (previewPaneBody) {
        previewPaneBody.innerHTML = `
            <div class="preview-path-bar">${escapeHtml(item.path)}</div>
            <div class="preview-empty-state">
                <p class="empty-title">CARGANDO VISTA PREVIA...</p>
                <p class="empty-subtitle">Preparando documento sin modificar el archivo original</p>
            </div>
        `;
    }

    const data = await fetchPreviewData(item);
    if (reqId !== previewRequestSeq) return;

    activePreviewData = data;
    const metaParts = [];
    if (data.extension) metaParts.push(data.extension.toUpperCase());
    if (data.size_label || item.size) metaParts.push(data.size_label || item.size);
    if (data.page_count) metaParts.push(`${data.page_count} págs`);
    if (data.modified) metaParts.push(data.modified);
    if (previewMeta) {
        previewMeta.textContent = metaParts.join(' · ') || item.path;
    }

    updatePreviewActionButtons(item, data);
    if (previewPaneBody) {
        renderPreviewInto(previewPaneBody, data, item);
    }
}

async function openQuickLook(item) {
    if (!item) return;
    modalTitle.textContent = item.name || 'VISTA RÁPIDA DE ARCHIVO';
    if (modalSubtitle) modalSubtitle.textContent = item.path || '';
    quickLookModal.classList.add('open');

    if (activePreviewItem && activePreviewItem.path === item.path && activePreviewData) {
        updatePreviewActionButtons(item, activePreviewData);
        renderPreviewInto(modalPreviewContainer, activePreviewData, item);
        return;
    }

    modalPreviewContainer.innerHTML = `<pre class="code-preview">// Extrayendo vista previa de ${escapeHtml(item.path)}...</pre>`;
    const data = await fetchPreviewData(item);
    activePreviewItem = item;
    activePreviewData = data;
    updatePreviewActionButtons(item, data);
    renderPreviewInto(modalPreviewContainer, data, item);
}

function renderPreviewInto(container, data, item) {
    if (!container) return;
    const filePath = data.file || item.path;
    const rawUrl = `${apiBase}/api/raw?file=${encodeURIComponent(filePath)}`;
    const pathBar = `<div class="preview-path-bar">${escapeHtml(filePath)}</div>`;

    if (data.kind === 'pdf') {
        const hasText = Boolean(data.content && data.content.trim().length > 0);
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <div class="preview-tab-group">
                    <button type="button" class="preview-tab-btn active" data-view="visual">📄 Visor PDF</button>
                    ${hasText ? '<button type="button" class="preview-tab-btn" data-view="text">📝 Texto Extraído</button>' : ''}
                </div>
                <span class="preview-pane-meta">${data.page_count ? `${data.page_count} páginas` : 'PDF'}</span>
            </div>
            <div class="preview-view-slot" style="flex:1;min-height:0;display:flex;flex-direction:column;">
                <iframe class="preview-pdf-frame" src="${rawUrl}#view=FitH" title="Vista previa PDF"></iframe>
            </div>
        `;
        const slot = container.querySelector('.preview-view-slot');
        container.querySelectorAll('.preview-tab-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                container.querySelectorAll('.preview-tab-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');
                if (btn.dataset.view === 'visual') {
                    slot.innerHTML = `<iframe class="preview-pdf-frame" src="${rawUrl}#view=FitH" title="Vista previa PDF"></iframe>`;
                } else {
                    slot.innerHTML = renderNumberedCodeHtml(data.content || '');
                }
            });
        });
        return;
    }

    if (data.kind === 'image') {
        container.innerHTML = `
            ${pathBar}
            <div class="preview-media-stage">
                <img src="${rawUrl}" alt="${escapeHtml(item.name)}" />
                <div class="preview-media-caption">Cargando dimensiones...</div>
            </div>
        `;
        const img = container.querySelector('img');
        const cap = container.querySelector('.preview-media-caption');
        img.addEventListener('load', () => {
            cap.textContent = `${img.naturalWidth} × ${img.naturalHeight} px · ${data.size_label || item.size || ''}`;
        });
        img.addEventListener('error', () => {
            cap.textContent = 'No se pudo cargar la imagen en el navegador.';
        });
        return;
    }

    if (data.kind === 'video') {
        container.innerHTML = `
            ${pathBar}
            <div class="preview-media-stage">
                <video controls preload="metadata" src="${rawUrl}"></video>
                <div class="preview-media-caption">${escapeHtml((data.extension || 'VIDEO').toUpperCase())} · ${escapeHtml(data.size_label || item.size || '')}</div>
            </div>
        `;
        return;
    }

    if (data.kind === 'audio') {
        container.innerHTML = `
            ${pathBar}
            <div class="preview-media-stage">
                <div style="font-size:38px;">🎵</div>
                <audio controls preload="metadata" src="${rawUrl}"></audio>
                <div class="preview-media-caption">${escapeHtml(item.name)} · ${escapeHtml(data.size_label || item.size || '')}</div>
            </div>
        `;
        return;
    }

    if (data.kind === 'markdown') {
        const mdText = data.content || '';
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <div class="preview-tab-group">
                    <button type="button" class="preview-tab-btn active" data-view="rendered">✨ Formateado</button>
                    <button type="button" class="preview-tab-btn" data-view="raw">⌨️ Markdown</button>
                </div>
                <span class="preview-pane-meta">${mdText.length.toLocaleString()} caracteres</span>
            </div>
            <div class="preview-view-slot">${renderSafeMarkdownHtml(mdText)}</div>
        `;
        const slot = container.querySelector('.preview-view-slot');
        container.querySelectorAll('.preview-tab-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                container.querySelectorAll('.preview-tab-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');
                slot.innerHTML = btn.dataset.view === 'rendered'
                    ? renderSafeMarkdownHtml(mdText)
                    : renderNumberedCodeHtml(mdText);
            });
        });
        return;
    }

    if (data.kind === 'word') {
        renderWordOfficePreview(container, data, item);
        return;
    }

    if (data.kind === 'excel') {
        renderExcelOfficePreview(container, data, item);
        return;
    }

    if (data.kind === 'powerpoint') {
        renderPowerPointOfficePreview(container, data, item);
        return;
    }

    if (data.kind === 'table' && Array.isArray(data.sheets) && data.sheets.length > 0) {
        const sheets = data.sheets;
        let currentSheetIdx = 0;
        const tabsHtml = sheets.map((s, i) =>
            `<button type="button" class="preview-tab-btn ${i === 0 ? 'active' : ''}" data-sheet="${i}">📊 ${escapeHtml(s.name)} (${s.total_rows})</button>`
        ).join('');
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <div class="preview-tab-group">${tabsHtml}</div>
                <span class="preview-pane-meta" id="sheetCountMeta"></span>
            </div>
            <div class="preview-table-wrap"></div>
        `;
        const wrap = container.querySelector('.preview-table-wrap');
        const metaEl = container.querySelector('#sheetCountMeta');
        const drawSheet = (idx) => {
            const sheet = sheets[idx];
            if (!sheet) return;
            metaEl.textContent = `Mostrando ${sheet.rows.length} de ${sheet.total_rows} filas`;
            wrap.innerHTML = renderTableHtml(sheet.rows);
        };
        drawSheet(0);
        container.querySelectorAll('.preview-tab-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                container.querySelectorAll('.preview-tab-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');
                currentSheetIdx = parseInt(btn.dataset.sheet, 10);
                drawSheet(currentSheetIdx);
            });
        });
        return;
    }

    if (data.kind === 'slides' && Array.isArray(data.slides) && data.slides.length > 0) {
        const cards = data.slides.map(slide => {
            const listItems = (slide.lines || []).map(l => `<li>${escapeHtml(l)}</li>`).join('');
            return `
                <div class="preview-slide-card">
                    <div class="preview-slide-header">Diapositiva ${slide.number} · ${escapeHtml(slide.title)}</div>
                    <div class="preview-slide-body">
                        ${listItems ? `<ul>${listItems}</ul>` : '<em>Sin texto adicional en esta diapositiva.</em>'}
                    </div>
                </div>
            `;
        }).join('');
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <span class="preview-pane-meta">📽️ ${data.slides.length} diapositivas extraídas</span>
            </div>
            <div class="preview-slides-list">${cards}</div>
        `;
        return;
    }

    if (data.kind === 'document') {
        const paras = Array.isArray(data.paragraphs) && data.paragraphs.length > 0
            ? data.paragraphs
            : (data.content || '').split(/\n+/).filter(Boolean);
        const parasHtml = paras.length > 0
            ? paras.map(p => `<p>${escapeHtml(p)}</p>`).join('')
            : '<p><em>Documento sin texto extraíble (posiblemente escaneado o protegido).</em></p>';
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <span class="preview-pane-meta">📝 Documento · ${(data.content || '').length.toLocaleString()} caracteres</span>
            </div>
            <div class="preview-doc-surface">${parasHtml}</div>
        `;
        return;
    }

    if (data.kind === 'archive' && Array.isArray(data.archive_entries)) {
        const rows = [['Archivo / Ruta interna', 'Tamaño', 'Comprimido']];
        data.archive_entries.forEach(e => {
            rows.push([
                e.path,
                e.is_dir ? '<DIR>' : formatBytesSimple(e.size),
                e.is_dir ? '-' : formatBytesSimple(e.compressed_size)
            ]);
        });
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <span class="preview-pane-meta">📦 Paquete comprimido · ${data.archive_entries.length} de ${data.archive_total} entradas</span>
            </div>
            <div class="preview-table-wrap">${renderTableHtml(rows)}</div>
        `;
        return;
    }

    if (data.kind === 'html') {
        const htmlSrc = data.content || '';
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <div class="preview-tab-group">
                    <button type="button" class="preview-tab-btn active" data-view="code">⌨️ Código HTML</button>
                    <button type="button" class="preview-tab-btn" data-view="sandbox">🌐 Vista Segura</button>
                </div>
                <span class="preview-pane-meta">${htmlSrc.length.toLocaleString()} caracteres</span>
            </div>
            <div class="preview-view-slot" style="flex:1;min-height:0;display:flex;flex-direction:column;">
                ${renderNumberedCodeHtml(htmlSrc)}
            </div>
        `;
        const slot = container.querySelector('.preview-view-slot');
        container.querySelectorAll('.preview-tab-btn').forEach(btn => {
            btn.addEventListener('click', () => {
                container.querySelectorAll('.preview-tab-btn').forEach(b => b.classList.remove('active'));
                btn.classList.add('active');
                if (btn.dataset.view === 'code') {
                    slot.innerHTML = renderNumberedCodeHtml(htmlSrc);
                } else {
                    slot.innerHTML = '<iframe class="preview-pdf-frame" sandbox="" title="Vista segura HTML"></iframe>';
                    slot.querySelector('iframe').srcdoc = htmlSrc;
                }
            });
        });
        return;
    }

    if (data.kind === 'code' || data.kind === 'fallback') {
        let text = data.content || '';
        if ((data.extension === 'json' || (item.name || '').toLowerCase().endsWith('.json')) && text.trim()) {
            try {
                text = JSON.stringify(JSON.parse(text), null, 2);
            } catch (_) {}
        }
        container.innerHTML = `
            ${pathBar}
            <div class="preview-toolbar-row">
                <span class="preview-pane-meta">⌨️ Texto / Código · ${text.length.toLocaleString()} caracteres</span>
            </div>
            ${text ? renderNumberedCodeHtml(text) : '<div class="preview-empty-state"><p class="empty-subtitle">Archivo vacío</p></div>'}
        `;
        return;
    }

    // Unsupported binary/CAD/design
    container.innerHTML = `
        ${pathBar}
        <div class="preview-unsupported-box">
            <div style="font-size:34px;margin-bottom:8px;">🗂️</div>
            <p class="empty-title">${escapeHtml(item.name)}</p>
            <div class="preview-meta-grid">
                <strong>Tipo:</strong><span>${escapeHtml((data.extension || item.badge || 'BIN').toUpperCase())}</span>
                <strong>Tamaño:</strong><span>${escapeHtml(data.size_label || item.size || '-')}</span>
                <strong>Modificado:</strong><span>${escapeHtml(data.modified || '-')}</span>
            </div>
            <p class="empty-subtitle">${escapeHtml(data.note || 'Vista previa web no disponible para este formato.')}</p>
            <div style="display:flex;gap:8px;justify-content:center;margin-top:12px;flex-wrap:wrap;">
                <button type="button" class="preview-action-btn" data-act="open">📂 Abrir Ubicación</button>
            </div>
        </div>
    `;
    const openBtn = container.querySelector('[data-act="open"]');
    if (openBtn) openBtn.addEventListener('click', () => openFileLocation(item));
}

function renderNumberedCodeHtml(text) {
    const lines = String(text || '').split(/\r?\n/);
    const maxLines = 1500;
    const slice = lines.slice(0, maxLines);
    const rows = slice.map((line, i) => `
        <div class="preview-code-row">
            <span class="preview-line-num">${i + 1}</span>
            <span class="preview-line-text">${escapeHtml(line)}</span>
        </div>
    `).join('');
    const truncNote = lines.length > maxLines
        ? `<div class="preview-pane-meta" style="margin-top:8px;">Mostrando primeras ${maxLines} de ${lines.length} líneas</div>`
        : '';
    return `<div class="preview-code-lines">${rows}${truncNote}</div>`;
}

function renderTableHtml(rows) {
    if (!rows || rows.length === 0) {
        return '<div class="preview-empty-state"><p class="empty-subtitle">Tabla vacía</p></div>';
    }
    const header = rows[0] || [];
    const body = rows.slice(1);
    const thead = `<thead><tr>${header.map(c => `<th>${escapeHtml(c)}</th>`).join('')}</tr></thead>`;
    const tbody = `<tbody>${body.map(r => `<tr>${r.map(c => `<td>${escapeHtml(c)}</td>`).join('')}</tr>`).join('')}</tbody>`;
    return `<table class="preview-table">${thead}${tbody}</table>`;
}

function safeHex(val) {
    return typeof val === 'string' && /^#[0-9A-Fa-f]{6}$/.test(val) ? val : null;
}

function renderWordRunsHtml(runs) {
    if (!Array.isArray(runs) || runs.length === 0) return '';
    return runs.map(r => {
        const styles = [];
        if (r.bold) styles.push('font-weight:700');
        if (r.italic) styles.push('font-style:italic');
        const decos = [];
        if (r.underline) decos.push('underline');
        if (r.strike) decos.push('line-through');
        if (decos.length) styles.push(`text-decoration:${decos.join(' ')}`);
        const col = safeHex(r.color);
        if (col) styles.push(`color:${col}`);
        if (typeof r.size_pt === 'number' && r.size_pt >= 8 && r.size_pt <= 48) {
            styles.push(`font-size:${r.size_pt.toFixed(1)}pt`);
        }
        const txt = escapeHtml(r.text || '').replace(/\n/g, '<br>');
        return styles.length ? `<span style="${styles.join(';')}">${txt}</span>` : txt;
    }).join('');
}

function renderWordParagraphHtml(p, filePath) {
    const cls = [];
    if (p.style && p.style !== 'normal') cls.push(`w-style-${p.style}`);
    if (p.is_list) cls.push('w-list-item');
    const align = ['left', 'center', 'right', 'justify'].includes(p.align) ? p.align : 'left';
    let inner = renderWordRunsHtml(p.runs);
    if (p.image_entry) {
        const imgUrl = `${apiBase}/api/zip-asset?file=${encodeURIComponent(filePath)}&entry=${encodeURIComponent(p.image_entry)}`;
        inner += `<img class="office-word-img" src="${imgUrl}" alt="Imagen del documento" />`;
    }
    return `<p class="${cls.join(' ')}" style="text-align:${align};">${inner || '&nbsp;'}</p>`;
}

function renderWordVisualHtml(wordVisual, filePath) {
    if (!wordVisual || !Array.isArray(wordVisual.blocks) || wordVisual.blocks.length === 0) {
        return '<div class="office-word-canvas"><div class="office-word-page"><p><em>Documento sin contenido visual extraíble.</em></p></div></div>';
    }
    const htmlBlocks = wordVisual.blocks.map(b => {
        if (b.kind === 'paragraph') {
            return renderWordParagraphHtml(b, filePath);
        }
        if (b.kind === 'table' && Array.isArray(b.rows)) {
            const rowsHtml = b.rows.map(row => {
                const cells = row.map(cell => {
                    const bg = safeHex(cell.bg);
                    const style = bg ? ` style="background:${bg};"` : '';
                    const span = cell.colspan > 1 ? ` colspan="${cell.colspan}"` : '';
                    const paras = (cell.paragraphs || []).map(p => renderWordParagraphHtml(p, filePath)).join('');
                    return `<td${span}${style}>${paras || '&nbsp;'}</td>`;
                }).join('');
                return `<tr>${cells}</tr>`;
            }).join('');
            return `<table class="office-word-table"><tbody>${rowsHtml}</tbody></table>`;
        }
        return '';
    }).join('');
    const truncNote = wordVisual.truncated
        ? '<p style="margin-top:16px;color:#64748B;font-size:11px;border-top:1px dashed #CBD5E1;padding-top:8px;">Vista previa parcial del inicio del documento. Usa "Formato Original (PDF)" para ver todas las páginas.</p>'
        : '';
    return `<div class="office-word-canvas"><div class="office-word-page">${htmlBlocks}${truncNote}</div></div>`;
}

function renderWordOfficePreview(container, data, item) {
    const filePath = data.file || item.path;
    const pdfUrl = `${apiBase}/api/office-pdf?file=${encodeURIComponent(filePath)}`;
    const pathBar = `<div class="preview-path-bar">${escapeHtml(filePath)}</div>`;
    const canPdf = Boolean(data.office_pdf_available);
    const hasVisual = Boolean(data.word_visual && Array.isArray(data.word_visual.blocks) && data.word_visual.blocks.length > 0);
    const startWithPdf = canPdf && (data.office_pdf_cached || data.legacy || !hasVisual);

    const reqSeqSnapshot = previewRequestSeq;
    let userManualTab = false;

    container.innerHTML = `
        ${pathBar}
        <div class="office-shell">
            <div class="office-brand-bar word">
                <div class="office-brand-title">
                    <span class="office-brand-icon">W</span>
                    <span>Microsoft Word · ${escapeHtml((data.extension || 'DOCX').toUpperCase())}</span>
                    <span class="office-pdf-badge" id="wordPdfStatus" ${canPdf ? '' : 'hidden'}>${startWithPdf ? 'PDF Exacto' : 'Cargando PDF...'}</span>
                </div>
                <div class="office-brand-tabs">
                    ${canPdf ? `<button type="button" class="office-tab-btn ${startWithPdf ? 'active' : ''}" data-tab="pdf">📐 Formato Original</button>` : ''}
                    <button type="button" class="office-tab-btn ${!startWithPdf ? 'active' : ''}" data-tab="visual">📝 Vista Word</button>
                    <button type="button" class="office-tab-btn" data-tab="text">⌨️ Texto</button>
                </div>
            </div>
            <div class="office-stage"></div>
        </div>
    `;

    const stage = container.querySelector('.office-stage');
    const statusBadge = container.querySelector('#wordPdfStatus');
    const setActiveTab = (tabName) => {
        container.querySelectorAll('.office-tab-btn').forEach(b => {
            b.classList.toggle('active', b.dataset.tab === tabName);
        });
        if (tabName === 'pdf') {
            stage.innerHTML = `<iframe class="preview-pdf-frame" style="border:none;border-radius:0;" src="${pdfUrl}#view=FitH" title="Word Formato Original"></iframe>`;
        } else if (tabName === 'visual') {
            if (hasVisual) {
                stage.innerHTML = renderWordVisualHtml(data.word_visual, filePath);
            } else {
                const paras = Array.isArray(data.paragraphs) && data.paragraphs.length > 0
                    ? data.paragraphs
                    : (data.content || '').split(/\n+/).filter(Boolean);
                const html = paras.length
                    ? paras.map(p => `<p>${escapeHtml(p)}</p>`).join('')
                    : '<p><em>Documento sin texto extraíble.</em></p>';
                stage.innerHTML = `<div class="office-word-canvas"><div class="office-word-page">${html}</div></div>`;
            }
        } else {
            stage.innerHTML = `<div style="padding:12px;overflow:auto;flex:1;">${renderNumberedCodeHtml(data.content || '')}</div>`;
        }
    };

    setActiveTab(startWithPdf ? 'pdf' : 'visual');

    container.querySelectorAll('.office-tab-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            userManualTab = true;
            setActiveTab(btn.dataset.tab);
        });
    });

    if (canPdf && !startWithPdf) {
        fetch(pdfUrl).then(res => {
            if (!res.ok) throw new Error('pdf failed');
            if (statusBadge) statusBadge.textContent = 'PDF Exacto listo';
            if (!userManualTab && reqSeqSnapshot === previewRequestSeq && container.isConnected) {
                setActiveTab('pdf');
            }
        }).catch(() => {
            if (statusBadge) statusBadge.hidden = true;
        });
    }
}

function excelColName(idx) {
    let n = idx + 1;
    let out = '';
    while (n > 0) {
        const rem = (n - 1) % 26;
        out = String.fromCharCode(65 + rem) + out;
        n = Math.floor((n - 1) / 26);
    }
    return out;
}

function renderExcelSheetGridHtml(sheet) {
    if (!sheet || !Array.isArray(sheet.rows) || sheet.rows.length === 0) {
        return '<div class="preview-empty-state"><p class="empty-subtitle">Hoja sin celdas con datos.</p></div>';
    }
    const widths = Array.isArray(sheet.col_widths) && sheet.col_widths.length > 0
        ? sheet.col_widths
        : (sheet.rows[0] || []).map(() => 112);

    const colGroup = `<colgroup><col style="width:42px;">${widths.map(w => `<col style="width:${w}px;">`).join('')}</colgroup>`;
    const colHeaders = `<thead><tr><th class="excel-corner"></th>${widths.map((_, idx) => `<th>${excelColName(idx)}</th>`).join('')}</tr></thead>`;
    const bodyRows = sheet.rows.map((row, rIdx) => {
        const cells = row.map((cell, cIdx) => {
            if (cell.hidden_by_merge) return '';
            const styles = [];
            if (cell.bold) styles.push('font-weight:700');
            if (cell.italic) styles.push('font-style:italic');
            const fg = safeHex(cell.color);
            if (fg) styles.push(`color:${fg}`);
            const bg = safeHex(cell.bg);
            if (bg) styles.push(`background:${bg}`);
            if (['left', 'center', 'right'].includes(cell.align)) {
                styles.push(`text-align:${cell.align}`);
            }
            const spans = `${cell.colspan > 1 ? ` colspan="${cell.colspan}"` : ''}${cell.rowspan > 1 ? ` rowspan="${cell.rowspan}"` : ''}`;
            const styleAttr = styles.length ? ` style="${styles.join(';')}"` : '';
            const ref = `${excelColName(cIdx)}${rIdx + 1}`;
            return `<td${spans}${styleAttr} data-ref="${ref}" data-val="${escapeHtml(cell.value || '')}" title="${escapeHtml(cell.value || '')}">${escapeHtml(cell.value || '')}</td>`;
        }).join('');
        return `<tr><th class="excel-row-hdr">${rIdx + 1}</th>${cells}</tr>`;
    }).join('');

    return `<table class="excel-grid">${colGroup}${colHeaders}<tbody>${bodyRows}</tbody></table>`;
}

function renderExcelOfficePreview(container, data, item) {
    const filePath = data.file || item.path;
    const pdfUrl = `${apiBase}/api/office-pdf?file=${encodeURIComponent(filePath)}`;
    const pathBar = `<div class="preview-path-bar">${escapeHtml(filePath)}</div>`;
    const canPdf = Boolean(data.office_pdf_available);
    const sheets = (data.excel_visual && Array.isArray(data.excel_visual.sheets) && data.excel_visual.sheets)
        || (Array.isArray(data.sheets) && data.sheets.map(s => ({
            name: s.name,
            col_widths: (s.rows[0] || []).map(() => 120),
            rows: s.rows.map((r, ri) => r.map(v => ({
                value: v,
                bold: ri === 0,
                italic: false,
                align: null,
                color: null,
                bg: ri === 0 ? '#F1F5F9' : null,
                colspan: 1,
                rowspan: 1,
                hidden_by_merge: false
            }))),
            total_rows: s.total_rows
        })))
        || [];

    const startWithPdf = canPdf && data.legacy && sheets.length === 0;

    container.innerHTML = `
        ${pathBar}
        <div class="office-shell">
            <div class="office-brand-bar excel">
                <div class="office-brand-title">
                    <span class="office-brand-icon">X</span>
                    <span>Microsoft Excel · ${escapeHtml((data.extension || 'XLSX').toUpperCase())}</span>
                </div>
                <div class="office-brand-tabs">
                    <button type="button" class="office-tab-btn ${!startWithPdf ? 'active' : ''}" data-tab="grid">📊 Hoja de Cálculo</button>
                    ${canPdf ? `<button type="button" class="office-tab-btn ${startWithPdf ? 'active' : ''}" data-tab="pdf">📐 Vista Impresión (PDF)</button>` : ''}
                    <button type="button" class="office-tab-btn" data-tab="text">⌨️ Texto</button>
                </div>
            </div>
            <div class="office-stage"></div>
        </div>
    `;

    const stage = container.querySelector('.office-stage');
    let activeSheetIdx = 0;

    const drawGridStage = () => {
        if (sheets.length === 0) {
            stage.innerHTML = `<div style="padding:12px;overflow:auto;flex:1;">${renderNumberedCodeHtml(data.content || 'Archivo sin hojas legibles.')}</div>`;
            return;
        }
        const sheetTabsHtml = sheets.map((s, idx) =>
            `<button type="button" class="excel-sheet-tab ${idx === activeSheetIdx ? 'active' : ''}" data-sheet="${idx}">${escapeHtml(s.name)} (${s.total_rows})</button>`
        ).join('');

        stage.innerHTML = `
            <div class="excel-formula-bar">
                <span class="excel-cell-ref" id="excelRefBox">A1</span>
                <span class="excel-fx-label">fx</span>
                <span class="excel-fx-value" id="excelFxVal">Haz clic en cualquier celda para ver su contenido completo</span>
            </div>
            <div class="excel-grid-wrap" id="excelGridWrap"></div>
            <div class="excel-sheet-tabs">${sheetTabsHtml}</div>
        `;
        const gridWrap = stage.querySelector('#excelGridWrap');
        const refBox = stage.querySelector('#excelRefBox');
        const fxVal = stage.querySelector('#excelFxVal');

        const paintSheet = (idx) => {
            activeSheetIdx = idx;
            const sh = sheets[idx];
            gridWrap.innerHTML = renderExcelSheetGridHtml(sh);
            gridWrap.querySelectorAll('td').forEach(td => {
                td.addEventListener('click', () => {
                    gridWrap.querySelectorAll('td.excel-cell-selected').forEach(c => c.classList.remove('excel-cell-selected'));
                    td.classList.add('excel-cell-selected');
                    refBox.textContent = td.dataset.ref || 'A1';
                    fxVal.textContent = td.dataset.val || '(vacía)';
                });
            });
            stage.querySelectorAll('.excel-sheet-tab').forEach(t => {
                t.classList.toggle('active', parseInt(t.dataset.sheet, 10) === idx);
            });
        };
        paintSheet(activeSheetIdx);
        stage.querySelectorAll('.excel-sheet-tab').forEach(t => {
            t.addEventListener('click', () => paintSheet(parseInt(t.dataset.sheet, 10)));
        });
    };

    const setTab = (tab) => {
        container.querySelectorAll('.office-tab-btn').forEach(b => b.classList.toggle('active', b.dataset.tab === tab));
        if (tab === 'grid') {
            drawGridStage();
        } else if (tab === 'pdf') {
            stage.innerHTML = `<iframe class="preview-pdf-frame" style="border:none;border-radius:0;" src="${pdfUrl}#view=FitH" title="Excel Vista PDF"></iframe>`;
        } else {
            stage.innerHTML = `<div style="padding:12px;overflow:auto;flex:1;">${renderNumberedCodeHtml(data.content || '')}</div>`;
        }
    };

    setTab(startWithPdf ? 'pdf' : 'grid');
    container.querySelectorAll('.office-tab-btn').forEach(btn => {
        btn.addEventListener('click', () => setTab(btn.dataset.tab));
    });
}

function renderPptSlideHtml(slide, aspectRatio, filePath) {
    const bg = safeHex(slide.bg) || '#FFFFFF';
    const els = (slide.elements || []).map(el => {
        const pos = `left:${el.left_pct.toFixed(2)}%;top:${el.top_pct.toFixed(2)}%;width:${el.width_pct.toFixed(2)}%;height:${el.height_pct.toFixed(2)}%;`;
        if (el.kind === 'image' && el.entry) {
            const imgUrl = `${apiBase}/api/zip-asset?file=${encodeURIComponent(filePath)}&entry=${encodeURIComponent(el.entry)}`;
            return `<img class="ppt-el-img" style="${pos}" src="${imgUrl}" alt="Imagen de diapositiva" />`;
        }
        if (el.kind === 'textbox') {
            const boxBg = safeHex(el.bg);
            const bgAttr = boxBg ? `background:${boxBg};` : '';
            const paras = (el.paragraphs || []).map(p => {
                const align = ['left', 'center', 'right', 'justify'].includes(p.align) ? p.align : 'left';
                const bullet = p.bullet ? '• ' : '';
                const defaultSize = el.role === 'title' ? 'clamp(14px, 2.2vw, 24px)' : el.role === 'subtitle' ? 'clamp(11px, 1.5vw, 17px)' : 'clamp(10px, 1.2vw, 14px)';
                const defaultWeight = el.role === 'title' ? '700' : '400';
                return `<p style="text-align:${align};font-size:${defaultSize};font-weight:${defaultWeight};">${bullet}${renderWordRunsHtml(p.runs)}</p>`;
            }).join('');
            return `<div class="ppt-el-box" style="${pos}${bgAttr}">${paras}</div>`;
        }
        return '';
    }).join('');
    return `<div class="ppt-slide-frame" style="aspect-ratio:${aspectRatio.toFixed(3)};background:${bg};">${els}</div>`;
}

function renderPowerPointOfficePreview(container, data, item) {
    const filePath = data.file || item.path;
    const pdfUrl = `${apiBase}/api/office-pdf?file=${encodeURIComponent(filePath)}`;
    const pathBar = `<div class="preview-path-bar">${escapeHtml(filePath)}</div>`;
    const canPdf = Boolean(data.office_pdf_available);
    const pptVisual = data.ppt_visual;
    const slides = (pptVisual && Array.isArray(pptVisual.slides) && pptVisual.slides) || [];
    const aspectRatio = (pptVisual && pptVisual.aspect_ratio) || 1.777;
    const hasVisual = slides.length > 0;
    const startWithPdf = canPdf && (data.office_pdf_cached || data.legacy || !hasVisual);

    const reqSeqSnapshot = previewRequestSeq;
    let userManualTab = false;
    let currentSlideIdx = 0;

    container.innerHTML = `
        ${pathBar}
        <div class="office-shell">
            <div class="office-brand-bar powerpoint">
                <div class="office-brand-title">
                    <span class="office-brand-icon">P</span>
                    <span>Microsoft PowerPoint · ${escapeHtml((data.extension || 'PPTX').toUpperCase())}</span>
                    <span class="office-pdf-badge" id="pptPdfStatus" ${canPdf ? '' : 'hidden'}>${startWithPdf ? 'PDF Exacto' : 'Cargando PDF...'}</span>
                </div>
                <div class="office-brand-tabs">
                    ${canPdf ? `<button type="button" class="office-tab-btn ${startWithPdf ? 'active' : ''}" data-tab="pdf">📐 Formato Original</button>` : ''}
                    <button type="button" class="office-tab-btn ${!startWithPdf ? 'active' : ''}" data-tab="slide">🎞️ Diapositiva</button>
                    ${hasVisual ? '<button type="button" class="office-tab-btn" data-tab="all">📋 Todas</button>' : ''}
                    <button type="button" class="office-tab-btn" data-tab="text">⌨️ Texto</button>
                </div>
            </div>
            <div class="office-stage"></div>
        </div>
    `;

    const stage = container.querySelector('.office-stage');
    const statusBadge = container.querySelector('#pptPdfStatus');

    const drawSingleSlide = () => {
        if (!hasVisual) {
            stage.innerHTML = `<div style="padding:12px;overflow:auto;flex:1;">${renderNumberedCodeHtml(data.content || 'Sin contenido extraíble.')}</div>`;
            return;
        }
        const s = slides[currentSlideIdx];
        const notesHtml = s.notes
            ? `<div class="ppt-notes-box"><strong>Notas del orador:</strong><br>${escapeHtml(s.notes)}</div>`
            : '';
        stage.innerHTML = `
            <div class="ppt-workspace">
                <div class="ppt-nav-bar">
                    <button type="button" class="ppt-nav-btn" id="pptPrevBtn" ${currentSlideIdx === 0 ? 'disabled' : ''}>◀ Anterior</button>
                    <span>Diapositiva ${currentSlideIdx + 1} de ${slides.length} · ${escapeHtml(s.title)}</span>
                    <button type="button" class="ppt-nav-btn" id="pptNextBtn" ${currentSlideIdx >= slides.length - 1 ? 'disabled' : ''}>Siguiente ▶</button>
                </div>
                ${renderPptSlideHtml(s, aspectRatio, filePath)}
                ${notesHtml}
            </div>
        `;
        const prevBtn = stage.querySelector('#pptPrevBtn');
        const nextBtn = stage.querySelector('#pptNextBtn');
        if (prevBtn) prevBtn.addEventListener('click', () => { currentSlideIdx = Math.max(0, currentSlideIdx - 1); drawSingleSlide(); });
        if (nextBtn) nextBtn.addEventListener('click', () => { currentSlideIdx = Math.min(slides.length - 1, currentSlideIdx + 1); drawSingleSlide(); });
    };

    const setTab = (tabName) => {
        container.querySelectorAll('.office-tab-btn').forEach(b => b.classList.toggle('active', b.dataset.tab === tabName));
        if (tabName === 'pdf') {
            stage.innerHTML = `<iframe class="preview-pdf-frame" style="border:none;border-radius:0;" src="${pdfUrl}#view=FitH" title="PowerPoint Formato Original"></iframe>`;
        } else if (tabName === 'slide') {
            drawSingleSlide();
        } else if (tabName === 'all') {
            const allHtml = slides.map(s => `
                <div>
                    <div class="ppt-nav-bar" style="margin-bottom:6px;"><span>Diapositiva ${s.number} · ${escapeHtml(s.title)}</span></div>
                    ${renderPptSlideHtml(s, aspectRatio, filePath)}
                </div>
            `).join('');
            stage.innerHTML = `<div class="ppt-workspace ppt-filmstrip">${allHtml}</div>`;
        } else {
            stage.innerHTML = `<div style="padding:12px;overflow:auto;flex:1;">${renderNumberedCodeHtml(data.content || '')}</div>`;
        }
    };

    setTab(startWithPdf ? 'pdf' : 'slide');
    container.querySelectorAll('.office-tab-btn').forEach(btn => {
        btn.addEventListener('click', () => {
            userManualTab = true;
            setTab(btn.dataset.tab);
        });
    });

    if (canPdf && !startWithPdf) {
        fetch(pdfUrl).then(res => {
            if (!res.ok) throw new Error('pdf failed');
            if (statusBadge) statusBadge.textContent = 'PDF Exacto listo';
            if (!userManualTab && reqSeqSnapshot === previewRequestSeq && container.isConnected) {
                setTab('pdf');
            }
        }).catch(() => {
            if (statusBadge) statusBadge.hidden = true;
        });
    }
}

function renderSafeMarkdownHtml(raw) {
    const lines = String(raw || '').split(/\r?\n/);
    let html = '';
    let inCode = false;
    let codeBuf = [];
    let inList = false;

    const closeList = () => {
        if (inList) {
            html += '</ul>';
            inList = false;
        }
    };

    const inlineFmt = (s) => {
        let out = escapeHtml(s);
        out = out.replace(/`([^`]+)`/g, '<code>$1</code>');
        out = out.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
        out = out.replace(/\*([^*]+)\*/g, '<em>$1</em>');
        out = out.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
        return out;
    };

    for (const line of lines) {
        if (line.trim().startsWith('```')) {
            closeList();
            if (!inCode) {
                inCode = true;
                codeBuf = [];
            } else {
                html += `<pre><code>${escapeHtml(codeBuf.join('\n'))}</code></pre>`;
                inCode = false;
            }
            continue;
        }
        if (inCode) {
            codeBuf.push(line);
            continue;
        }
        const trimmed = line.trim();
        if (!trimmed) {
            closeList();
            continue;
        }
        const hMatch = trimmed.match(/^(#{1,4})\s+(.+)$/);
        if (hMatch) {
            closeList();
            const lvl = hMatch[1].length;
            html += `<h${lvl}>${inlineFmt(hMatch[2])}</h${lvl}>`;
            continue;
        }
        if (trimmed.startsWith('> ')) {
            closeList();
            html += `<blockquote>${inlineFmt(trimmed.slice(2))}</blockquote>`;
            continue;
        }
        if (/^[-*+]\s+/.test(trimmed)) {
            if (!inList) {
                html += '<ul>';
                inList = true;
            }
            html += `<li>${inlineFmt(trimmed.replace(/^[-*+]\s+/, ''))}</li>`;
            continue;
        }
        closeList();
        html += `<p>${inlineFmt(trimmed)}</p>`;
    }
    if (inCode) {
        html += `<pre><code>${escapeHtml(codeBuf.join('\n'))}</code></pre>`;
    }
    closeList();
    return `<div class="preview-doc-surface preview-md-surface">${html}</div>`;
}

function formatBytesSimple(bytes) {
    if (!bytes || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB'];
    const exp = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
    return `${(bytes / Math.pow(1024, exp)).toFixed(1)} ${units[exp]}`;
}

if (libreofficeNoticeCloseBtn) {
    libreofficeNoticeCloseBtn.addEventListener('click', () => {
        libreofficeNoticeDismissed = true;
        if (libreofficeNoticeBanner) libreofficeNoticeBanner.hidden = true;
    });
}
if (previewOpenLocationBtn) {
    previewOpenLocationBtn.addEventListener('click', () => openFileLocation(activePreviewItem));
}
if (previewExpandBtn) {
    previewExpandBtn.addEventListener('click', () => {
        if (activePreviewItem) openQuickLook(activePreviewItem);
    });
}
if (previewCopyBtn) {
    previewCopyBtn.addEventListener('click', async () => {
        if (!activePreviewData || !activePreviewData.content) return;
        try {
            await navigator.clipboard.writeText(activePreviewData.content);
            const orig = previewCopyBtn.textContent;
            previewCopyBtn.textContent = '✅ Copiado';
            setTimeout(() => { previewCopyBtn.textContent = orig; }, 1200);
        } catch (_) {}
    });
}
if (modalOpenLocationBtn) {
    modalOpenLocationBtn.addEventListener('click', () => openFileLocation(activePreviewItem));
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
        loadPreview(currentResults[bestIdx]);
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
            loadPreview(radarBlips[selectedIndex].item);
        }
    } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        if (radarBlips.length > 0) {
            selectedIndex = (selectedIndex - 1 + radarBlips.length) % radarBlips.length;
            selectResult(selectedIndex);
            loadPreview(radarBlips[selectedIndex].item);
        }
    } else if (e.key === ' ') {
        if (document.activeElement !== searchInput && selectedIndex >= 0 && selectedIndex < radarBlips.length) {
            e.preventDefault();
            openQuickLook(radarBlips[selectedIndex].item);
        }
    } else if (e.key === 'Enter') {
        if (selectedIndex >= 0 && selectedIndex < radarBlips.length) {
            openFileLocation(radarBlips[selectedIndex].item);
        }
    } else if (e.key === 'Escape') {
        quickLookModal.classList.remove('open');
    }
});

// Listener de Búsqueda con debounce ligero (120ms)
searchInput.addEventListener('input', (e) => {
    const val = e.target.value;
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
        refreshExtensionChips(val);
        performSearch(val);
    }, 120);
});

// Iniciar radar y búsqueda inicial
performSearch('');
drawRadar();
