import { useCallback, useState } from "react";
import { Download, ImagePlus, Loader2, Search } from "lucide-react";
import { api } from "../../api/PondApiClient";
import { ErrorBanner } from "../../components/shared";
import type { HfModel, HfModelFile } from "../../api/types";
import { formatBytes, formatSize } from "./modelsView";

/** Search Hugging Face for a model to add. A file with a known picture add-on says its size first. */
export function AddBand({
  carriesPictures,
  onStarted,
}: {
  /** False when this device will not carry picture support; null when nothing says. */
  carriesPictures: boolean | null;
  /** A download began; `message` is the pond's own sentence about what it will fetch. */
  onStarted: (message: string | undefined) => void;
}) {
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [results, setResults] = useState<HfModel[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [openRepo, setOpenRepo] = useState<string | null>(null);
  const [files, setFiles] = useState<Record<string, HfModelFile[]>>({});
  const [loadingFiles, setLoadingFiles] = useState<string | null>(null);
  const [starting, setStarting] = useState<string | null>(null);
  const [withPictures, setWithPictures] = useState(true);

  const search = useCallback(async () => {
    const q = query.trim();
    if (!q) return;
    setSearching(true);
    setError(null);
    setOpenRepo(null);
    try {
      setResults((await api.searchGgufModels(q)).models ?? []);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setResults(null);
    } finally {
      setSearching(false);
    }
  }, [query]);

  async function openFiles(repoId: string) {
    if (openRepo === repoId) {
      setOpenRepo(null);
      return;
    }
    setOpenRepo(repoId);
    if (files[repoId]) return;
    setLoadingFiles(repoId);
    try {
      // Await outside the updater: an async setState callback would store a promise as the list.
      const list = (await api.listHfModelFiles(repoId)).files ?? [];
      setFiles((f) => ({ ...f, [repoId]: list }));
    } catch {
      setFiles((f) => ({ ...f, [repoId]: [] }));
    } finally {
      setLoadingFiles(null);
    }
  }

  async function download(file: HfModelFile) {
    setStarting(file.filename);
    try {
      const started = await api.downloadModelFromUrl(file.url, "gguf", file.filename, {
        pictures: withPictures,
      });
      onStarted(started?.message);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setStarting(null);
    }
  }

  /** What a file would cost: its own size, and its add-on's where it has one and it is wanted. The
   *  listing often leaves a file's own size out; the pond states it when the download starts. */
  function costOf(file: HfModelFile): string {
    const own = formatSize(file.size_mb) || "Size not listed";
    const pictures = carriesPictures === false ? null : file.pictures;
    return pictures && withPictures ? `${own} + ${formatBytes(pictures.size_bytes)} for pictures` : own;
  }

  const repoFiles = openRepo ? (files[openRepo] ?? []) : [];
  const offersPictures = carriesPictures !== false && repoFiles.some((f) => f.pictures);

  return (
    <section className="mdl-band" id="mdl-add">
      <h3 className="mdl-band__subtitle">Add a model</h3>
      <p className="mdl-band__sub">Search Hugging Face. Downloads land on this device and nowhere else.</p>

      <form className="mdl-search" onSubmit={(e) => { e.preventDefault(); void search(); }}>
        <Search size={16} aria-hidden="true" />
        <input className="mdl-search__input" type="search" value={query}
          aria-label="Search Hugging Face for a model"
          placeholder="gemma, qwen, whisper…"
          onChange={(e) => setQuery(e.target.value)} />
        <button type="submit" className="mm-btn" disabled={searching || !query.trim()}>
          {searching ? <Loader2 size={15} className="mm-spin" aria-hidden="true" /> : null}
          <span>{searching ? "Searching…" : "Search"}</span>
        </button>
      </form>

      {error && <ErrorBanner error={error} />}

      {results !== null && results.length === 0 && !searching && (
        <p className="mdl-empty">Nothing on Hugging Face matches “{query.trim()}”.</p>
      )}

      {results !== null && results.length > 0 && (
        <ul className="mdl-results">
          {results.map((r) => (
            <li key={r.id} className="mdl-result">
              <button type="button" className="mdl-result__head"
                onClick={() => void openFiles(r.id)} aria-expanded={openRepo === r.id}>
                <span className="mdl-result__name">{r.id}</span>
                <span className="mdl-result__meta">
                  {typeof r.downloads === "number" ? `${r.downloads.toLocaleString()} downloads` : ""}
                </span>
              </button>

              {openRepo === r.id && (
                <div className="mdl-result__files">
                  {loadingFiles === r.id && <span className="mdl-muted">Reading the file list…</span>}
                  {loadingFiles !== r.id && repoFiles.length === 0 && (
                    <span className="mdl-muted">No GGUF files in this repository.</span>
                  )}
                  {offersPictures && (
                    <label className="mm-choice">
                      <input type="checkbox" checked={withPictures}
                        onChange={(e) => setWithPictures(e.target.checked)} />
                      <ImagePlus size={14} aria-hidden="true" />
                      <span>Include picture support where a file has it</span>
                    </label>
                  )}
                  {repoFiles.map((f) => (
                    <div key={f.filename} className="mdl-file">
                      <span className="mdl-file__name" title={f.filename}>{f.filename}</span>
                      <span className="mdl-file__size">{costOf(f)}</span>
                      <button type="button" className="mm-btn"
                        onClick={() => void download(f)} disabled={starting === f.filename}
                        aria-label={`Download ${f.filename}`}>
                        <Download size={15} aria-hidden="true" />
                        <span>{starting === f.filename ? "Starting…" : "Download"}</span>
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
