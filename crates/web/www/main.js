import init, { App, load_vanilla, minecraft_versions } from "./pkg/web.js";

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
const packNames = [];

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
        if (report == null)
          status("Schematic loaded; waiting for block assets…");
        return report;
      });
    } else if (/\.(zip|jar)$/i.test(file.name)) {
      await attempt(() => {
        const report = app.add_pack(bytes);
        packNames.unshift(file.name);
        ui.packs.textContent = packNames.join(" › ");
        ui.clearPacks.disabled = false;
        return report;
      });
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
ui.clearPacks.addEventListener("click", () =>
  attempt(() => {
    packNames.length = 0;
    ui.packs.textContent = "";
    ui.clearPacks.disabled = true;
    return app.clear_packs();
  }),
);
ui.version.addEventListener("change", () => loadVersion(ui.version.value));

ui.exportGlb.addEventListener("click", () =>
  attempt(() => (download(app.export_glb(), "glb", "model/gltf-binary"), null)),
);
ui.exportFbx.addEventListener("click", () =>
  attempt(
    () => (download(app.export_fbx(), "fbx", "application/octet-stream"), null),
  ),
);

let dragDepth = 0;
window.addEventListener("dragenter", (e) => {
  e.preventDefault();
  dragDepth++;
  document.body.classList.add("dragging");
});
window.addEventListener("dragleave", () => {
  if (--dragDepth === 0) document.body.classList.remove("dragging");
});
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove("dragging");
  openFiles(e.dataTransfer.files);
});

try {
  const versions = await minecraft_versions();
  ui.version.replaceChildren(...versions.map((v) => new Option(v, v)));
  await loadVersion(versions[0]);
} catch (e) {
  status(`Could not reach Mojang's version list: ${e.message ?? e}`, true);
}
