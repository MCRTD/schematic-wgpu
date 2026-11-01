const CACHE_NAME = "schematic-viewer-v1";

async function cached(key, load) {
  const cache = globalThis.caches
    ? await caches.open(CACHE_NAME).catch(() => null)
    : null;
  const hit = cache && (await cache.match(key));
  if (hit) return hit;
  const response = await load();
  if (cache) await cache.put(key, response.clone());
  return response;
}

export async function fetch_range(url, start, end) {
  const key = `${url}?range=${start}-${end}`;
  const response = await cached(key, async () => {
    const r = await fetch(url, {
      headers: { Range: `bytes=${start}-${end - 1}` },
    });
    if (r.status !== 206 && r.status !== 200)
      throw new Error(`HTTP ${r.status} fetching ${url}`);
    let bytes = new Uint8Array(await r.arrayBuffer());
    if (r.status === 200) bytes = bytes.slice(start, end);
    return new Response(bytes);
  });
  return new Uint8Array(await response.arrayBuffer());
}

export async function fetch_text(url, useCache) {
  const load = async () => {
    const r = await fetch(url);
    if (!r.ok) throw new Error(`HTTP ${r.status} fetching ${url}`);
    return r;
  };
  const response = useCache ? await cached(url, load) : await load();
  return response.text();
}

export function next_task() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}
