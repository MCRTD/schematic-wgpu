import init, { App, load_vanilla, minecraft_versions } from "./pkg/web.js";

// Rust panics are reported with a JS stack; the default 10 frames stop
// before the frame that caused them.
Error.stackTraceLimit = 64;

const $ = (id) => document.getElementById(id);
const ui = {
  canvas: $("view"),
  drop: $("drop"),
  file: $("file"),
  version: $("version"),
  addPack: $("add-pack"),
  clearPacks: $("clear-packs"),
  packFile: $("pack-file"),
  packs: $("packs"),
  exportGlb: $("export"),
  exportFbx: $("export-fbx"),
  layer: $("layer"),
  time: $("time"),
  layerLabel: $("layer-label"),
  status: $("status"),
  report: $("report"),
};

function status(message, isError = false) {
  ui.status.textContent = message;
  ui.status.classList.toggle("error", isError);
}

function showReport(report) {
  if (report == null) return;
  ui.report.replaceChildren(
    ...report.split("\n").map((line) => {
      const div = document.createElement("div");
      div.textContent = line;
      if (line.startsWith("⚠")) div.className = "warn";
      return div;
    }),
  );
  ui.exportGlb.disabled = false;
  ui.exportFbx.disabled = false;
  // A different model starts fully shown; a re-mesh of the same one keeps its layer.
  const count = app.layer_count;
  if (count !== Number(ui.layer.max)) {
    ui.layer.max = count;
    ui.layer.value = count;
  }
  ui.layer.disabled = count === 0;
  showLayer();
}

// The top layer shows everything, so it needs no cut.
function showLayer() {
  const layer = Number(ui.layer.value);
  const count = Number(ui.layer.max);
  app.set_layer(layer < count ? layer : undefined);
  ui.layerLabel.textContent = layer < count ? `${layer} / ${count}` : `all ${count}`;
}

function stepLayer(steps) {
  if (ui.layer.disabled) return;
  ui.layer.value = Number(ui.layer.value) + steps;
  showLayer();
}

function download(bytes, extension, type) {
  const link = document.createElement("a");
  link.href = URL.createObjectURL(new Blob([bytes], { type }));
  link.download = `${app.schematic_name || "schematic"}.${extension}`;
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 10_000);
}

async function attempt(action) {
  try {
    showReport(await action());
  } catch (e) {
    console.error(e);
    status(String(e.message ?? e), true);
  }
}

await init();
const app = new App(ui.canvas);
let haveVanilla = false;

// Packs and mod jars, highest priority first, over the Minecraft assets.
// Reorder by dragging or with the arrows.
function renderPacks() {
  const names = app.packs;
  const item = (text, className) => {
    const li = document.createElement("li");
    const name = document.createElement("span");
    name.className = "name";
    name.textContent = name.title = text;
    li.append(name);
    if (className) li.className = className;
    return li;
  };
  const button = (text, label, disabled, action) => {
    const b = document.createElement("button");
    b.textContent = text;
    b.title = b.ariaLabel = label;
    b.disabled = disabled;
    b.addEventListener("click", () => changePacks(action));
    return b;
  };
  const rows = names.map((text, i) => {
    const li = item(text);
    li.draggable = true;
    li.append(
      button("↑", "Higher priority", i === 0, () => app.move_pack(i, i - 1)),
      button("↓", "Lower priority", i === names.length - 1, () => app.move_pack(i, i + 1)),
      button("✕", "Remove", false, () => app.remove_pack(i)),
    );
    li.addEventListener("dragstart", (e) => e.dataTransfer.setData("text/x-pack", String(i)));
    li.addEventListener("dragover", (e) => {
      if (!e.dataTransfer.types.includes("text/x-pack")) return;
      e.preventDefault();
      li.classList.add("over");
    });
    li.addEventListener("dragleave", () => li.classList.remove("over"));
    li.addEventListener("drop", (e) => {
      const from = e.dataTransfer.getData("text/x-pack");
      if (from === "") return;
      e.preventDefault();
      e.stopPropagation();
      li.classList.remove("over");
      if (Number(from) !== i) changePacks(() => app.move_pack(Number(from), i));
    });
    return li;
  });
  const base = names.length ? [item(`Minecraft ${ui.version.value || ""}`.trim(), "base")] : [];
  ui.packs.replaceChildren(...rows, ...base);
  ui.clearPacks.disabled = names.length === 0;
}

function changePacks(action) {
  return attempt(() => {
    const report = action();
    renderPacks();
    return report;
  });
}

// Large builds are meshed and shown piece by piece.
app.set_progress((fraction) => {
  ui.report.textContent = `Building the model… ${Math.round(fraction * 100)}%`;
  ui.exportGlb.disabled = true;
  ui.exportFbx.disabled = true;
});

// Only the latest request for vanilla assets wins if the user switches fast.
let versionRequest = 0;
async function loadVersion(version) {
  const request = ++versionRequest;
  ui.version.disabled = true;
  await attempt(async () => {
    const assets = await load_vanilla(
      version,
      (msg) => request === versionRequest && status(msg),
    );
    if (request !== versionRequest) return null;
    status(
      `Minecraft ${assets.version}: ${assets.files} block assets (${(assets.downloaded / 1e6).toFixed(1)} MB, cached for next time)`,
    );
    haveVanilla = true;
    // Shows the version under the packs.
    renderPacks();
    return app.set_vanilla(assets);
  });
  ui.version.disabled = false;
}

async function openFiles(files) {
  for (const file of files) {
    const bytes = new Uint8Array(await file.arrayBuffer());
    if (/\.litematic$/i.test(file.name)) {
      await attempt(() => {
        const report = app.load_schematic(bytes);
        if (!haveVanilla && app.packs.length === 0)
          status("Schematic loaded; waiting for block assets…");
        return report;
      });
    } else if (/\.(zip|jar)$/i.test(file.name)) {
      await changePacks(() => app.add_pack(file.name, bytes));
    } else {
      status(`Unsupported file: ${file.name}`, true);
    }
  }
}

ui.drop.addEventListener("click", () => ui.file.click());
ui.drop.addEventListener(
  "keydown",
  (e) => (e.key === "Enter" || e.key === " ") && ui.file.click(),
);
ui.file.addEventListener("change", () => openFiles(ui.file.files));
ui.addPack.addEventListener("click", () => ui.packFile.click());
ui.packFile.addEventListener("change", () => openFiles(ui.packFile.files));
ui.clearPacks.addEventListener("click", () => changePacks(() => app.clear_packs()));
ui.version.addEventListener("change", () => loadVersion(ui.version.value));
ui.layer.addEventListener("input", showLayer);
ui.time.addEventListener("change", () => app.set_time(ui.time.value));
ui.canvas.addEventListener("keydown", (e) => {
  const steps = { PageUp: 1, "]": 1, PageDown: -1, "[": -1 }[e.key];
  if (steps) {
    e.preventDefault();
    stepLayer(steps);
  } else if (e.key === "Escape") {
    stepLayer(Number(ui.layer.max));
  }
});

ui.exportGlb.addEventListener("click", () =>
  attempt(() => (download(app.export_glb(), "glb", "model/gltf-binary"), null)),
);
ui.exportFbx.addEventListener("click", () =>
  attempt(
    () => (download(app.export_fbx(), "fbx", "application/octet-stream"), null),
  ),
);

// Files dropped anywhere are opened; reordering packs drags no files.
const hasFiles = (e) => e.dataTransfer?.types.includes("Files");
let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth++;
  document.body.classList.add("dragging");
});
window.addEventListener("dragleave", (e) => {
  if (hasFiles(e) && --dragDepth === 0) document.body.classList.remove("dragging");
});
window.addEventListener("dragover", (e) => hasFiles(e) && e.preventDefault());
window.addEventListener("drop", (e) => {
  if (!hasFiles(e)) return;
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove("dragging");
  openFiles(e.dataTransfer.files);
});

let versions = null;
try {
  versions = await minecraft_versions();
} catch (e) {
  status(`Could not reach Mojang's version list: ${e.message ?? e}`, true);
}
if (versions) {
  ui.version.replaceChildren(...versions.map((v) => new Option(v, v)));
  await loadVersion(versions[0]);
}
