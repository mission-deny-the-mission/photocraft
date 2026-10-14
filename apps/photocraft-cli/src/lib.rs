//! Headless Photocraft command line. `run` is the whole program, so it can be
//! tested in-process as well as through the binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::Write;
use std::path::{Path, PathBuf};

use photocraft_automation::{AuthorizedWorkspace, Headless, PhotocraftMcp, files, security};
use photocraft_engine::actions_cmds::Action;
use photocraft_io::ExportOptions;
use photocraft_storage::{StorageBackend, WriteOptions};
use serde_json::{Value, json};

pub const USAGE: &str = "\
photocraft-cli: headless Photocraft

USAGE:
  photocraft-cli convert <in> <out> [--format <ext>] [--quality <1-100>] [--tiff-layers] [--tga-bits <24|32>] [--dpx-bits <8|10|12|16>] [--store …]
      Convert between formats (.pcraft, .psd, .png, .jpg, .tif, .webp, .exr, …).
      TIFF output is flat unless --tiff-layers keeps the layers (Photoshop layer data).
      --quality sets the JPEG or WebP quality; a WebP written with a quality is lossy, without one lossless.
      --tga-bits sets a TGA's bits per pixel: 32 writes the alpha channel (or else the transparency) as
      its alpha, 24 none. Without it: 32 with an alpha channel or transparency, else 24.
      --dpx-bits sets a DPX's bits per sample. Without it: 8 for an 8-bit document, else 16.
  photocraft-cli info <file> [--compact] [--store …]
      Print the document as JSON (size, mode, depth, layer tree).
  photocraft-cli run (<file> | --new <json>) --cmd <id> [--params <json>] [--cmd …] [--out <file>] [--format <ext>] [--quality <1-100>] [--tiff-layers] [--tga-bits <24|32>] [--dpx-bits <8|10|12|16>] [--store …]
      Open a file, run engine commands in order, save the result. Each --params
      applies to the preceding --cmd. Prints each command's JSON result.
  photocraft-cli batch --actions <actions.json|set.atn> [--action <name>] --in <dir> --out <dir> [--format <ext>] [--quality <1-100>] [--in-place] [--tiff-layers] [--tga-bits <24|32>] [--dpx-bits <8|10|12|16>] [--store …]
      Apply an action list to every image in a directory. A Photoshop Actions file (.atn) is a
      set: its action --action names plays, as below. JSON steps are [id, params] pairs,
      {\"command\": id, \"params\": {…}} objects or bare ids, as a recorded action or droplet stores them
      (a list, or wrapped in {\"actions\": …}, {\"steps\": …} or a droplet). An Actions set (the Actions
      panel's actions.json, {\"actions\": [{\"name\", \"steps\"}…]}) plays the action --action names (needed
      when the set holds more than one) with the whole set loaded, so its steps can play other actions
      of the set. An --out folder that is the --in folder is refused, as the results would replace
      the originals; --in-place allows it.
  photocraft-cli droplet <file.pcdroplet> <file-or-dir>… [--out <dir>]
      Run a droplet (File › Automate › Create Droplet) on images and folders.
  photocraft-cli commands [--json] [--filter <text>]
      List the engine command registry.
  photocraft-cli mcp [--bridge <127.0.0.1:port>] [--control-token <64-hex> | --control-token-file <path>]
      [--automation-read-root <dir>] [--automation-write-root <dir>]
      Run the MCP server on stdio (headless engine, or bridge to a running `photocraft --control <port>`).
  photocraft-cli serve [--port <port>] [--control-token <64-hex> | --control-token-file <path>]
      [--automation-read-root <dir>] [--automation-write-root <dir>]
      Keep one headless session open and answer JSON lines ({\"id\",\"method\",\"params\"}) on stdio,
      or on 127.0.0.1:<port>. Methods: engine.execute, jobs.list/cancel, engine.commands,
      doc.open/new/place/save/exportLayers/exportDataSets/importDataSets/inspect/render/select/close, session.list, batch, methods
      (docs/control-protocol.md#headless-server).

  photocraft-cli store-write PATH FILE [--store local --store-root DIR | --store webdav
                    --store-url URL [--store-user U --store-password P | --store-token T]] [--etag TAG] [--atomic]
      Upload a local file to storage at PATH (--etag writes only if the stored version
      still carries TAG).
  photocraft-cli store-read PATH OUT [--store …]
      Download storage PATH to local file OUT.
  photocraft-cli store-stat PATH [--store …]
      Metadata (size, ETag) as JSON.
  photocraft-cli store-list [PREFIX] [--store …]
      Direct children as JSON.
  photocraft-cli store-remove PATH [--store …]
      Delete (missing: success).
  photocraft-cli store-probe [--store …]
      Capabilities + server probe as JSON.
  photocraft-cli store-recover [--store …]
      Finish or drop interrupted writes, as JSON.
      --store-root is a local/NAS/synced folder; --store-url is the WebDAV collection
      (plain http only: LAN, VPN or SSH tunnel; https is refused).
      WebDAV auth is Basic (--store-user/--store-password) or an OIDC Bearer token
      (--store-token, or $CRAFT_DAV_TOKEN for k8s secretKeyRef), never both.
      convert, info, run and batch take the same --store* flags: with them, their file
      arguments are storage paths (downloaded, processed, uploaded). For batch, --in and
      --out are storage prefixes; --actions stays a local file.
  photocraft-cli <subcommand> --help (or -h) prints this text. A flag the subcommand doesn't take is
  an error.
";

struct Args {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

/// A subcommand: the flags it reads (taking a value, or given bare) and what it runs. A flag
/// outside these is a usage error, so a typo such as `--fromat` can't be silently ignored (#423).
struct Subcommand {
    name: &'static str,
    values: &'static [&'static str],
    bare: &'static [&'static str],
    run: fn(&Args, &mut dyn Write, &mut dyn Write) -> R,
}

const SUBCOMMANDS: &[Subcommand] = &[
    Subcommand {
        name: "convert",
        values: &[
            "--format",
            "--quality",
            "--tga-bits",
            "--dpx-bits",
            "--store",
            "--store-root",
            "--store-url",
            "--store-user",
            "--store-password",
            "--store-token",
            "--etag",
        ],
        bare: &["--tiff-layers", "--atomic"],
        run: convert,
    },
    Subcommand {
        name: "info",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &["--compact"],
        run: |a, out, _| info(a, out),
    },
    Subcommand {
        name: "run",
        values: &[
            "--new",
            "--cmd",
            "--params",
            "--out",
            "--format",
            "--quality",
            "--tga-bits",
            "--dpx-bits",
            "--store",
            "--store-root",
            "--store-url",
            "--store-user",
            "--store-password",
            "--store-token",
            "--etag",
        ],
        bare: &["--tiff-layers", "--atomic"],
        run: run_cmds,
    },
    Subcommand {
        name: "batch",
        values: &[
            "--actions",
            "--action",
            "--in",
            "--out",
            "--format",
            "--quality",
            "--tga-bits",
            "--dpx-bits",
            "--store",
            "--store-root",
            "--store-url",
            "--store-user",
            "--store-password",
            "--store-token",
        ],
        bare: &["--in-place", "--tiff-layers"],
        run: batch,
    },
    Subcommand {
        name: "store-write",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token", "--etag"],
        bare: &["--atomic"],
        run: store_write,
    },
    Subcommand {
        name: "store-read",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_read,
    },
    Subcommand {
        name: "store-stat",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_stat,
    },
    Subcommand {
        name: "store-list",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_list,
    },
    Subcommand {
        name: "store-remove",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_remove,
    },
    Subcommand {
        name: "store-probe",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_probe,
    },
    Subcommand {
        name: "store-recover",
        values: &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"],
        bare: &[],
        run: store_recover,
    },
    Subcommand { name: "droplet", values: &["--out"], bare: &[], run: droplet },
    Subcommand { name: "commands", values: &["--filter"], bare: &["--json"], run: |a, out, _| commands(a, out) },
    Subcommand {
        name: "mcp",
        values: &["--bridge", "--control-token", "--control-token-file", "--automation-read-root", "--automation-write-root"],
        bare: &[],
        run: |a, _, _| mcp(a),
    },
    Subcommand {
        name: "serve",
        values: &["--port", "--control-token", "--control-token-file", "--automation-read-root", "--automation-write-root"],
        bare: &[],
        run: |a, _, err| serve(a, err),
    },
];

/// The subcommand's arguments, or `None` when they ask for help (`--help` or `-h`).
fn parse(sub: &Subcommand, args: &[String]) -> Result<Option<Args>, String> {
    let mut a = Args { positional: Vec::new(), flags: Vec::new() };
    let unknown = |flag: &str| format!("unknown flag {flag} for {}", sub.name);
    let mut i = 0;
    while let Some(s) = args.get(i) {
        if s == "--help" || s == "-h" {
            return Ok(None);
        }
        if let Some((k, v)) = s.split_once('=').filter(|(k, _)| k.starts_with("--")) {
            if sub.bare.contains(&k) {
                return Err(format!("{k} takes no value"));
            }
            if !sub.values.contains(&k) {
                return Err(unknown(k));
            }
            a.flags.push((k.to_owned(), Some(v.to_owned())));
        } else if sub.values.contains(&s.as_str()) {
            let v = args.get(i + 1).ok_or_else(|| format!("{s} needs a value"))?;
            a.flags.push((s.clone(), Some(v.clone())));
            i += 1;
        } else if sub.bare.contains(&s.as_str()) {
            a.flags.push((s.clone(), None));
        } else if s.starts_with("--") {
            return Err(unknown(s));
        } else {
            a.positional.push(s.clone());
        }
        i += 1;
    }
    Ok(Some(a))
}

impl Args {
    fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(f, _)| f == k).and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(f, _)| f == k)
    }
}

type R = Result<(), String>;

/// [`run`] on the raw process arguments. An argument that is not valid Unicode is a usage error
/// naming its position and a lossy rendering (exit 2), never a panic: `std::env::args()` aborts
/// the process on one (issue #1108).
pub fn run_os(args: &[std::ffi::OsString], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let mut strings = Vec::with_capacity(args.len());
    for (i, a) in args.iter().enumerate() {
        match a.clone().into_string() {
            Ok(s) => strings.push(s),
            Err(raw) => {
                let _ = writeln!(err, "error: argument {} is not valid Unicode: `{}`\n\n{USAGE}", i + 1, raw.to_string_lossy());
                return 2;
            }
        }
    }
    run(&strings, out, err)
}

/// Run the CLI. Returns the process exit code (0 ok, 1 failure, 2 usage).
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some(cmd) = args.first() else {
        let _ = write!(err, "{USAGE}");
        return 2;
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            let _ = write!(out, "{USAGE}");
            return 0;
        }
        "--version" | "version" => {
            let _ = writeln!(out, "photocraft-cli {}", photocraft_engine::build_info::long_version());
            return 0;
        }
        _ => {}
    }
    let Some(sub) = SUBCOMMANDS.iter().find(|s| s.name == cmd) else {
        let _ = writeln!(err, "error: unknown command `{cmd}`\n\n{USAGE}");
        return 2;
    };
    let parsed = match parse(sub, &args[1..]) {
        Ok(Some(p)) => p,
        // `<subcommand> --help`: usage, without reading inputs or starting a server.
        Ok(None) => {
            let _ = write!(out, "{USAGE}");
            return 0;
        }
        Err(e) => {
            let _ = writeln!(err, "error: {e}\n\n{USAGE}");
            return 2;
        }
    };
    match (sub.run)(&parsed, out, err) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(err, "error: {e}");
            1
        }
    }
}

fn export_opts(a: &Args) -> Result<ExportOptions, String> {
    // TIFF output is flat unless asked: scripted conversions keep predictable sizes.
    let mut o = ExportOptions { tiff_layers: a.has("--tiff-layers"), ..Default::default() };
    if let Some(q) = a.get("--quality") {
        let q = q.parse().ok().filter(|q| (1..=100).contains(q)).ok_or_else(|| format!("bad --quality `{q}`: expected a whole number from 1 to 100"))?;
        o.encode.jpeg_quality = q;
        // Asking for a quality asks for a lossy WebP; the default WebP stays lossless.
        o.encode.webp_quality = q;
        o.encode.webp_lossless = false;
    }
    if let Some(b) = a.get("--tga-bits") {
        o.tga_bits = Some(b.parse().ok().and_then(photocraft_io::TgaBits::from_bits).ok_or_else(|| format!("bad --tga-bits `{b}`: expected 24 or 32"))?);
    }
    if let Some(b) = a.get("--dpx-bits") {
        o.encode.dpx_bits =
            Some(b.parse().ok().and_then(photocraft_io::DpxBits::from_bits).ok_or_else(|| format!("bad --dpx-bits `{b}`: expected 8, 10, 12 or 16"))?);
    }
    Ok(o)
}

fn warn_all(err: &mut dyn Write, ws: &[String]) {
    for w in ws {
        let _ = writeln!(err, "warning: {w}");
    }
}

/// The engine knows which type families aren't installed. Warn before an export
/// uses a fallback face rather than silently changing the appearance of the text.
fn missing_font_warnings(fonts: Vec<String>) -> Vec<String> {
    fonts.into_iter().map(|font| format!("font '{font}' is not installed; text may render using a fallback face")).collect()
}

fn automation_workspace(args: &Args) -> Result<AuthorizedWorkspace, String> {
    AuthorizedWorkspace::new(args.get("--automation-read-root").map(Path::new), args.get("--automation-write-root").map(Path::new))
        .map_err(|error| error.to_string())
}

/// Flags naming a storage backend (same names as wordcraft-storage, issue #518). Any of them
/// on convert/info/run/batch addresses storage paths instead of local files.
const STORE_FLAGS: &[&str] = &["--store", "--store-root", "--store-url", "--store-user", "--store-password", "--store-token"];

fn store_requested(a: &Args) -> bool {
    STORE_FLAGS.iter().any(|f| a.has(f))
}

/// Open the storage backend named by the `--store*` flags: `local` (a folder at `--store-root`)
/// or `webdav` (a collection at `--store-url`, Basic auth via `--store-user`/`--store-password`
/// or an OIDC/Bearer token via `--store-token`; the two schemes are mutually exclusive). The
/// token may arrive via `$CRAFT_DAV_TOKEN` (k8s secretKeyRef) instead of argv.
fn open_store(cmd: &str, a: &Args) -> Result<Box<dyn StorageBackend>, String> {
    match a.get("--store").unwrap_or("local") {
        "local" => {
            let root = a.get("--store-root").ok_or_else(|| format!("{cmd}: --store local needs --store-root DIR"))?;
            if a.get("--store-url").is_some() {
                return Err(format!("{cmd}: --store-url is for --store webdav, not local"));
            }
            photocraft_storage::local::LocalBackend::open(Path::new(root))
                .map(|b| Box::new(b) as Box<dyn StorageBackend>)
                .map_err(|e| format!("{cmd}: {e}"))
        }
        "webdav" => {
            let url = a.get("--store-url").ok_or_else(|| format!("{cmd}: --store webdav needs --store-url URL"))?;
            if a.get("--store-root").is_some() {
                return Err(format!("{cmd}: --store-root is for --store local, not webdav"));
            }
            let token = a
                .get("--store-token")
                .map(str::to_owned)
                .or_else(|| std::env::var("CRAFT_DAV_TOKEN").ok().filter(|t| !t.is_empty()));
            if token.is_some() && (a.get("--store-user").is_some() || a.get("--store-password").is_some()) {
                return Err(format!("{cmd}: --store-token cannot be combined with --store-user/--store-password (pick one auth scheme)"));
            }
            let backend = match (token, a.get("--store-user"), a.get("--store-password")) {
                (Some(t), None, None) => photocraft_storage::webdav::WebDavBackend::with_bearer(url, &t),
                (None, user, password) => photocraft_storage::webdav::WebDavBackend::new(url, user, password),
                _ => return Err(format!("{cmd}: --store-token cannot be combined with --store-user/--store-password (pick one auth scheme)")),
            };
            backend.map(|b| Box::new(b) as Box<dyn StorageBackend>).map_err(|e| format!("{cmd}: {e}"))
        }
        other => Err(format!("{cmd}: unknown --store `{other}` (local or webdav)")),
    }
}

fn store_write_opts(a: &Args) -> WriteOptions {
    WriteOptions { if_match: a.get("--etag").map(str::to_owned), atomic: a.has("--atomic") }
}

/// Scratch folder for storage downloads/uploads, removed on drop. File names keep the storage
/// path's base name so format detection (by extension) sees the same name.
struct TempStoreDir {
    path: PathBuf,
}

impl TempStoreDir {
    fn create(cmd: &str) -> Result<TempStoreDir, String> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("photocraft-cli-store-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&path).map_err(|e| format!("{cmd}: {}: {e}", path.display()))?;
        Ok(TempStoreDir { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempStoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn storage_basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or("doc")
}

/// Download `storage_path` into `dir`/`sub`, keeping its base name.
fn fetch(store: &dyn StorageBackend, cmd: &str, storage_path: &str, dir: &Path, sub: &str) -> Result<PathBuf, String> {
    let bytes = store.read(storage_path).map_err(|e| format!("{storage_path}: {e}"))?;
    let d = dir.join(sub);
    std::fs::create_dir_all(&d).map_err(|e| format!("{cmd}: {}: {e}", d.display()))?;
    let local = d.join(storage_basename(storage_path));
    std::fs::write(&local, &bytes).map_err(|e| format!("{cmd}: {}: {e}", local.display()))?;
    Ok(local)
}

/// Upload the local file at `local` to `storage_path`.
fn deposit(store: &dyn StorageBackend, cmd: &str, local: &Path, storage_path: &str, opts: &WriteOptions) -> Result<(), String> {
    let bytes = std::fs::read(local).map_err(|e| format!("{cmd}: {}: {e}", local.display()))?;
    store.write(storage_path, &bytes, opts).map_err(|e| format!("{storage_path}: {e}"))?;
    Ok(())
}

fn store_write(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-write", a, out, err)
}

fn store_read(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-read", a, out, err)
}

fn store_stat(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-stat", a, out, err)
}

fn store_list(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-list", a, out, err)
}

fn store_remove(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-remove", a, out, err)
}

fn store_probe(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-probe", a, out, err)
}

fn store_recover(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    store_command("store-recover", a, out, err)
}

fn store_command(cmd: &str, a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let store = open_store(cmd, a)?;
    match cmd {
        "store-write" => {
            let [path, file] = a.positional.as_slice() else {
                return Err("store-write needs PATH FILE".into());
            };
            let bytes = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
            let meta = store.write(path, &bytes, &store_write_opts(a)).map_err(|e| format!("{path}: {e}"))?;
            print_json(out, &json!({"path": path, "size": meta.size, "etag": meta.etag}), false)?;
            let _ = writeln!(err, "wrote {path} ({} bytes)", bytes.len());
            Ok(())
        }
        "store-read" => {
            let [path, dest] = a.positional.as_slice() else {
                return Err("store-read needs PATH OUT".into());
            };
            let bytes = store.read(path).map_err(|e| format!("{path}: {e}"))?;
            std::fs::write(dest, &bytes).map_err(|e| format!("{dest}: {e}"))?;
            let _ = writeln!(err, "wrote {dest} ({} bytes)", bytes.len());
            Ok(())
        }
        "store-stat" => {
            let [path] = a.positional.as_slice() else {
                return Err("store-stat needs PATH".into());
            };
            let meta = store.stat(path).map_err(|e| format!("{path}: {e}"))?;
            print_json(
                out,
                &json!({"path": path, "size": meta.size, "etag": meta.etag, "modified": meta.modified, "isDir": meta.is_dir}),
                false,
            )
        }
        "store-list" => {
            if a.positional.len() > 1 {
                return Err("store-list takes at most one PREFIX".into());
            }
            let prefix = a.positional.first().cloned().unwrap_or_default();
            let entries = store
                .list(&prefix)
                .map_err(|e| format!("{}: {e}", if prefix.is_empty() { "(root)" } else { &prefix }))?;
            let list: Vec<Value> =
                entries.iter().map(|e| json!({"path": e.path, "size": e.meta.size, "etag": e.meta.etag, "isDir": e.meta.is_dir})).collect();
            print_json(out, &Value::Array(list), false)
        }
        "store-remove" => {
            let [path] = a.positional.as_slice() else {
                return Err("store-remove needs PATH".into());
            };
            store.remove(path).map_err(|e| format!("{path}: {e}"))?;
            let _ = writeln!(err, "removed {path}");
            Ok(())
        }
        "store-probe" => {
            if !a.positional.is_empty() {
                return Err("store-probe takes no positional arguments".into());
            }
            let caps = store.capabilities();
            let probe = store.probe().map_err(|e| e.to_string())?;
            print_json(
                out,
                &json!({
                    "atomicWrite": caps.atomic_write, "conditionalWrite": caps.conditional_write, "list": caps.list,
                    "etag": caps.etag, "persistent": caps.persistent,
                    "davClasses": probe.dav_classes, "notes": probe.notes,
                }),
                false,
            )
        }
        _ => {
            // store-recover: finish or drop interrupted writes; safe at every startup.
            if !a.positional.is_empty() {
                return Err("store-recover takes no positional arguments".into());
            }
            let report = store.recover().map_err(|e| e.to_string())?;
            print_json(out, &json!({"completed": report.completed, "dropped": report.dropped, "removedTemp": report.removed_temp}), false)
        }
    }
}

fn convert(a: &Args, _out: &mut dyn Write, err: &mut dyn Write) -> R {
    let [input, output] = a.positional.as_slice() else {
        return Err("convert needs <in> <out>".into());
    };
    let opts = export_opts(a)?;
    if store_requested(a) {
        // Storage paths: download, convert locally, upload.
        let store = open_store("convert", a)?;
        let tmp = TempStoreDir::create("convert")?;
        let local_in = fetch(&*store, "convert", input, tmp.path(), "in")?;
        let local_out = tmp.path().join("out").join(storage_basename(output));
        if let Some(parent) = local_out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let o = files::open(&local_in).map_err(|e| e.to_string())?;
        warn_all(err, &o.warnings);
        warn_all(err, &photocraft_engine::type_extra_cmds::missing_font_warnings(&o.document));
        let ws = files::save(&o.document, &local_out, a.get("--format"), &opts, None).map_err(|e| e.to_string())?;
        warn_all(err, &ws);
        deposit(&*store, "convert", &local_out, output, &store_write_opts(a))?;
        return Ok(());
    }
    let o = files::open(Path::new(input)).map_err(|e| e.to_string())?;
    warn_all(err, &o.warnings);
    warn_all(err, &photocraft_engine::type_extra_cmds::missing_font_warnings(&o.document));
    let ws = files::save(&o.document, Path::new(output), a.get("--format"), &opts, None).map_err(|e| e.to_string())?;
    warn_all(err, &ws);
    Ok(())
}

fn print_json(out: &mut dyn Write, v: &Value, compact: bool) -> R {
    let s = if compact { serde_json::to_string(v) } else { serde_json::to_string_pretty(v) }.map_err(|e| e.to_string())?;
    writeln!(out, "{s}").map_err(|e| e.to_string())
}

fn info(a: &Args, out: &mut dyn Write) -> R {
    let [file] = a.positional.as_slice() else {
        return Err("info needs <file>".into());
    };
    if store_requested(a) {
        let store = open_store("info", a)?;
        let tmp = TempStoreDir::create("info")?;
        let local = fetch(&*store, "info", file, tmp.path(), "in")?;
        return info_path(&local, file, a, out);
    }
    info_path(Path::new(file), file, a, out)
}

fn info_path(local: &Path, label: &str, a: &Args, out: &mut dyn Write) -> R {
    let mut h = Headless::trusted_local();
    let opened = h.open(local).map_err(|e| e.to_string())?;
    let mut doc = h.inspect(None).map_err(|e| e.to_string())?;
    let mut warnings = opened["warnings"].as_array().cloned().unwrap_or_default();
    if let Some(st) = h.session.active() {
        warnings.extend(missing_font_warnings(photocraft_engine::type_extra_cmds::missing_fonts(&st.doc)).into_iter().map(Value::String));
    }
    doc["warnings"] = Value::Array(warnings);
    doc["file"] = json!(label);
    // Drop per-session noise.
    if let Value::Object(m) = &mut doc {
        for k in ["history", "canUndo", "canRedo", "revision"] {
            m.remove(k);
        }
    }
    print_json(out, &doc, a.has("--compact"))
}

/// `(id, params)` pairs from repeated `--cmd`/`--params` flags.
fn command_list(a: &Args) -> Result<Vec<(String, Value)>, String> {
    let mut cmds: Vec<(String, Value)> = Vec::new();
    for (k, v) in &a.flags {
        match (k.as_str(), v) {
            ("--cmd", Some(id)) => cmds.push((id.clone(), json!({}))),
            ("--params", Some(p)) => {
                let last = cmds.last_mut().ok_or("--params must follow a --cmd")?;
                last.1 = serde_json::from_str(p).map_err(|e| format!("bad --params JSON for `{}`: {e}", last.0))?;
            }
            _ => {}
        }
    }
    Ok(cmds)
}

fn run_cmds(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let opts = export_opts(a)?;
    // With --store* flags the <file> and --out arguments are storage paths: stage through a
    // scratch folder, then upload the result.
    let tmp: Option<TempStoreDir> = if store_requested(a) { Some(TempStoreDir::create("run")?) } else { None };
    let store: Option<Box<dyn StorageBackend>> = if tmp.is_some() { Some(open_store("run", a)?) } else { None };
    let mut h = Headless::trusted_local();
    // Opening, running commands and exporting can all notice the same missing font:
    // one `run` invocation reports each warning line at most once.
    let mut reported_fonts = std::collections::HashSet::<String>::new();
    match (a.positional.as_slice(), a.get("--new")) {
        ([file], None) => {
            let local: PathBuf = match (&tmp, &store) {
                (Some(t), Some(s)) => fetch(&**s, "run", file, t.path(), "in")?,
                _ => PathBuf::from(file),
            };
            let o = h.open(&local).map_err(|e| e.to_string())?;
            let ws: Vec<String> = serde_json::from_value(o["warnings"].clone()).unwrap_or_default();
            reported_fonts.extend(ws.iter().cloned());
            warn_all(err, &ws);
            reported_fonts.extend(ws);
        }
        ([], Some(new)) => {
            let p: Value = serde_json::from_str(new).map_err(|e| format!("bad --new JSON: {e}"))?;
            h.command_run("file.new", p).map_err(|e| format!("--new: {e}"))?;
        }
        _ => return Err("run needs exactly one of <file> or --new <json>".into()),
    }
    let cmds = command_list(a)?;
    if cmds.is_empty() && a.get("--out").is_none() {
        return Err("run needs at least one --cmd (or --out)".into());
    }
    for (id, params) in cmds {
        let r = h.command_run(&id, params).map_err(|e| format!("`{id}`: {e}"))?;
        if let Some(e) = photocraft_engine::actions_cmds::nested_failure(&id, &r) {
            return Err(format!("`{id}`: {e}"));
        }
        let new_warnings: Vec<String> = h
            .session
            .active()
            .map(|st| missing_font_warnings(photocraft_engine::type_extra_cmds::missing_fonts(&st.doc)))
            .unwrap_or_default()
            .into_iter()
            .filter(|w| reported_fonts.insert(w.clone()))
            .collect();
        warn_all(err, &new_warnings);
        let mut result = json!({"command": id, "result": r});
        if !new_warnings.is_empty() {
            result["warnings"] = json!(new_warnings);
        }
        print_json(out, &result, true)?;
    }
    // A `run` with only `--out` still reports missing fonts before exporting.
    let remaining: Vec<String> = h
        .session
        .active()
        .map(|st| missing_font_warnings(photocraft_engine::type_extra_cmds::missing_fonts(&st.doc)))
        .unwrap_or_default()
        .into_iter()
        .filter(|w| reported_fonts.insert(w.clone()))
        .collect();
    warn_all(err, &remaining);
    if let Some(o) = a.get("--out") {
        match (&tmp, &store) {
            (Some(t), Some(s)) => {
                let local_out = t.path().join("out").join(storage_basename(o));
                if let Some(parent) = local_out.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                let r = h.save(None, Some(&local_out), a.get("--format"), &opts).map_err(|e| e.to_string())?;
                let ws: Vec<String> = serde_json::from_value(r["warnings"].clone()).unwrap_or_default();
                let ws: Vec<String> = ws.into_iter().filter(|w| reported_fonts.insert(w.clone())).collect();
                warn_all(err, &ws);
                deposit(&**s, "run", &local_out, o, &store_write_opts(a))?;
            }
            _ => {
                let r = h.save(None, Some(Path::new(o)), a.get("--format"), &opts).map_err(|e| e.to_string())?;
                let ws: Vec<String> = serde_json::from_value(r["warnings"].clone()).unwrap_or_default();
                let ws: Vec<String> = ws.into_iter().filter(|w| reported_fonts.insert(w.clone())).collect();
                warn_all(err, &ws);
            }
        }
    }
    Ok(())
}

/// Parse an actions file: the steps of a recorded action or a droplet (`[id, params]` pairs,
/// `{"command": id, "params": {…}}` objects or bare ids; `"id"` is accepted for `"command"`), as a
/// list or wrapped in `{"actions": […]}`, `{"steps": […]}` or a droplet (#489).
pub fn parse_actions(text: &str) -> Result<Vec<(String, Value)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("actions JSON: {e}"))?;
    steps_of(&v)
}

fn steps_of(v: &Value) -> Result<Steps, String> {
    photocraft_engine::automate_cmds::parse_action(v.get("actions").unwrap_or(v), "batch --actions").map_err(|e| e.to_string())
}

/// Engine commands to run in order: `(id, params)`.
pub type Steps = Vec<(String, Value)>;

/// The steps `batch` plays on each file, and the actions they can call. An Actions set (the
/// Actions panel's `actions.json`: `{"actions": [{"name", "steps"}…]}`, or the bare list) plays
/// the action `name` picks (optional when the set holds one) with the whole set loaded, as
/// Photoshop's Batch does; anything else is one action's steps ([`parse_actions`]) (#2786).
pub fn load_actions(text: &str, name: Option<&str>) -> Result<(Steps, Vec<Action>), String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("actions JSON: {e}"))?;
    let list = v.get("actions").unwrap_or(&v);
    let is_set = list.as_array().is_some_and(|a| !a.is_empty() && a.iter().all(|x| x.get("name").is_some_and(Value::is_string)));
    if !is_set {
        if name.is_some() {
            return Err("--action picks an action from an Actions set ({\"actions\": [{\"name\", \"steps\"}…]}), but this file holds one action's steps".into());
        }
        return Ok((steps_of(&v)?, Vec::new()));
    }
    let set: Vec<Action> = serde_json::from_value(list.clone()).map_err(|e| format!("Actions set: {e}"))?;
    let names = || set.iter().map(|a| format!("`{}`", a.name)).collect::<Vec<_>>().join(", ");
    let action = match (name, set.as_slice()) {
        (Some(n), _) => set.iter().find(|a| a.name == n).ok_or_else(|| format!("no action named `{n}` in the set (it holds {})", names()))?,
        (None, [only]) => only,
        (None, _) => return Err(format!("the Actions set holds {} actions: pick one with --action <name> ({})", set.len(), names())),
    };
    let play = vec![("actions.play".to_string(), json!({"action": action.name}))];
    Ok((play, set))
}

/// [`load_actions`] for a file on disk: a Photoshop Actions file (`.atn`, by extension or by
/// its version word) is converted first; the played action's steps with no PhotoCraft
/// equivalent come back as warnings (they fail at playback, as in Photoshop when a step can't run).
pub fn load_actions_file(path: &str, name: Option<&str>) -> Result<(Steps, Vec<Action>, Vec<String>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let is_atn = Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("atn")) || bytes.starts_with(&[0, 0, 0, 16]);
    if !is_atn {
        let text = String::from_utf8(bytes).map_err(|e| format!("{path}: {e}"))?;
        let (steps, set) = load_actions(&text, name)?;
        return Ok((steps, set, Vec::new()));
    }
    let imp = photocraft_engine::atn_cmds::import(&bytes).map_err(|e| format!("{path}: {e}"))?;
    let names = || imp.actions.iter().map(|a| format!("`{}`", a.name)).collect::<Vec<_>>().join(", ");
    let action = match (name, imp.actions.as_slice()) {
        (Some(n), _) => {
            imp.actions.iter().find(|a| a.name == n).ok_or_else(|| format!("no action named `{n}` in the set `{}` (it holds {})", imp.set, names()))?
        }
        (None, [only]) => only,
        (None, []) => return Err(format!("{path}: the set holds no actions")),
        (None, _) => return Err(format!("the set `{}` holds {} actions: pick one with --action <name> ({})", imp.set, imp.actions.len(), names())),
    };
    let warnings = imp
        .unsupported
        .iter()
        .filter(|u| u.action == action.name)
        .map(|u| format!("`{}` step {}: Photoshop's \"{}\" ({}) has no PhotoCraft equivalent yet", u.action, u.step + 1, u.name, u.event))
        .chain(imp.warnings.iter().cloned())
        .collect();
    let play = vec![("actions.play".to_string(), json!({"action": action.name}))];
    Ok((play, imp.actions, warnings))
}

/// Whether `a` and `b` name the same existing folder, however they are spelt (relative, with a
/// trailing separator, through a symbolic link).
fn same_dir(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn known_extension(e: &str) -> bool {
    matches!(e, "pcraft" | "psd" | "psb" | "pdn" | "ora" | "kra" | "krz" | "af" | "afdesign" | "afphoto" | "afpub")
        || photocraft_codecs::from_extension(e).is_some_and(|f| photocraft_codecs::caps(f).read)
}

fn is_input(p: &Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| known_extension(&e.to_string_lossy().to_ascii_lowercase()))
}

/// A storage key (no local file to probe) counts as a batch input when its extension is a
/// readable image/document format.
fn is_store_input(key: &str) -> bool {
    Path::new(storage_basename(key)).extension().is_some_and(|e| known_extension(&e.to_string_lossy().to_ascii_lowercase()))
}

fn batch(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    if store_requested(a) {
        return batch_via_store(a, out, err);
    }
    let actions_path = a.get("--actions").ok_or("batch needs --actions <file>")?;
    let in_dir = PathBuf::from(a.get("--in").ok_or("batch needs --in <dir>")?);
    let out_dir = PathBuf::from(a.get("--out").ok_or("batch needs --out <dir>")?);
    let (actions, set, warnings) = load_actions_file(actions_path, a.get("--action"))?;
    for w in &warnings {
        let _ = writeln!(err, "warning: {w}");
    }
    let opts = export_opts(a)?;
    // Results are saved as `<out>/<stem>.<ext>`, so an `--out` that is the `--in` folder would
    // replace the originals (#492).
    if !a.has("--in-place") && same_dir(&in_dir, &out_dir) {
        return Err(format!(
            "--out {} is the --in folder, so the results would replace the originals: choose another --out folder, or pass --in-place to overwrite them",
            out_dir.display()
        ));
    }
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let mut inputs: Vec<PathBuf> =
        std::fs::read_dir(&in_dir).map_err(|e| format!("{}: {e}", in_dir.display()))?.flatten().map(|e| e.path()).filter(|p| is_input(p)).collect();
    inputs.sort();
    let (mut ok, mut failed) = (0, 0);
    let mut written = photocraft_engine::file_cmds::OutputClaims::default();
    // `--format .jpg` names outputs `<stem>.jpg`, as `--format jpg` does (#490).
    let format = a.get("--format").map(|f| f.strip_prefix('.').unwrap_or(f));
    for input in &inputs {
        let ext = format.map(str::to_owned).or_else(|| input.extension().map(|e| e.to_string_lossy().into_owned())).unwrap_or_else(|| "png".into());
        let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let target = out_dir.join(format!("{stem}.{ext}"));
        let target_text = target.to_string_lossy();
        let r = apply_batch_actions(&target_text, input, &target, &ext, &actions, &set, &opts, &mut written);
        match r {
            Ok(ws) => {
                ok += 1;
                written.record(&target_text, &input.to_string_lossy());
                let _ = writeln!(out, "ok    {} -> {}", input.display(), target.display());
                warn_all(err, &ws);
            }
            Err(e) => {
                failed += 1;
                let _ = writeln!(err, "FAIL  {}: {e}", input.display());
            }
        }
    }
    let _ = writeln!(out, "{ok} succeeded, {failed} failed");
    if failed > 0 { Err(format!("{failed} file(s) failed")) } else { Ok(()) }
}

/// Open one image, play the batch action list, save the result. `claim` is the logical output
/// name guarded against duplicates within the run (the local target path, or the storage key).
fn apply_batch_actions(
    claim: &str,
    input: &Path,
    target: &Path,
    ext: &str,
    actions: &Steps,
    set: &[Action],
    opts: &ExportOptions,
    written: &mut photocraft_engine::file_cmds::OutputClaims,
) -> Result<Vec<String>, String> {
    written.check(claim)?;
    let mut h = Headless::trusted_local();
    // Steps that play another action find it in the set; the session's playback stack
    // stops an action that calls itself.
    h.session.actions.list = set.to_vec();
    h.open(input).map_err(|e| e.to_string())?;
    for (id, p) in actions {
        if photocraft_engine::actions_cmds::shell_view_command(id) || photocraft_engine::actions_cmds::shell_save_command(id) {
            continue;
        }
        let r = h.command_run(id, p.clone()).map_err(|e| format!("`{id}`: {e}"))?;
        // A failed step inside a called action makes this file an error, not a saved result.
        if let Some(e) = photocraft_engine::actions_cmds::nested_failure(id, &r) {
            return Err(format!("`{id}`: {e}"));
        }
    }
    let r = h.save(None, Some(target), Some(ext), opts).map_err(|e| e.to_string())?;
    Ok(serde_json::from_value(r["warnings"].clone()).unwrap_or_default())
}

/// `batch` with `--store*` flags: `--in`/`--out` are storage prefixes. Each input is downloaded,
/// processed, and uploaded one file at a time; `--actions` stays a local file.
fn batch_via_store(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let store = open_store("batch", a)?;
    let actions_path = a.get("--actions").ok_or("batch needs --actions <file>")?;
    let in_prefix = a.get("--in").ok_or("batch needs --in <dir>")?.to_owned();
    let out_prefix = a.get("--out").ok_or("batch needs --out <dir>")?.to_owned();
    let (actions, set, warnings) = load_actions_file(actions_path, a.get("--action"))?;
    for w in &warnings {
        let _ = writeln!(err, "warning: {w}");
    }
    let opts = export_opts(a)?;
    if !a.has("--in-place") && in_prefix == out_prefix {
        return Err(format!(
            "--out {out_prefix} is the --in folder, so the results would replace the originals: choose another --out folder, or pass --in-place to overwrite them"
        ));
    }
    let entries = store.list(&in_prefix).map_err(|e| format!("{in_prefix}: {e}"))?;
    let mut names: Vec<String> =
        entries.into_iter().filter(|e| !e.meta.is_dir && is_store_input(&e.path)).map(|e| e.path).collect();
    names.sort();
    let tmp = TempStoreDir::create("batch")?;
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let (mut ok, mut failed) = (0, 0);
    let mut written = photocraft_engine::file_cmds::OutputClaims::default();
    // `--format .jpg` names outputs `<stem>.jpg`, as `--format jpg` does (#490).
    let format = a.get("--format").map(|f| f.strip_prefix('.').unwrap_or(f));
    for (i, name) in names.iter().enumerate() {
        let base = storage_basename(name);
        let ext =
            format.map(str::to_owned).or_else(|| Path::new(base).extension().map(|e| e.to_string_lossy().into_owned())).unwrap_or_else(|| "png".into());
        let stem = Path::new(base).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let target_key = format!("{}/{stem}.{ext}", out_prefix.trim_end_matches('/'));
        let r = (|| -> Result<Vec<String>, String> {
            let local_in = fetch(&*store, "batch", name, tmp.path(), "in")?;
            // Staging names stay unique per file so duplicate-output detection sees the logical keys.
            let local_out = out_dir.join(format!("{i:06}-{stem}.{ext}"));
            let ws = apply_batch_actions(&target_key, &local_in, &local_out, &ext, &actions, &set, &opts, &mut written)?;
            written.record(&target_key, name);
            deposit(&*store, "batch", &local_out, &target_key, &WriteOptions::default())?;
            Ok(ws)
        })();
        match r {
            Ok(ws) => {
                ok += 1;
                let _ = writeln!(out, "ok    {name} -> {target_key}");
                warn_all(err, &ws);
            }
            Err(e) => {
                failed += 1;
                let _ = writeln!(err, "FAIL  {name}: {e}");
            }
        }
    }
    let _ = writeln!(out, "{ok} succeeded, {failed} failed");
    if failed > 0 { Err(format!("{failed} file(s) failed")) } else { Ok(()) }
}

fn droplet(a: &Args, out: &mut dyn Write, err: &mut dyn Write) -> R {
    let (file, inputs) = a.positional.split_first().ok_or("droplet needs <file.pcdroplet> and inputs")?;
    if inputs.is_empty() {
        return Err("droplet needs at least one input file or folder".into());
    }
    let mut p = json!({"droplet": file, "input": inputs});
    if let Some(o) = a.get("--out") {
        p["output"] = json!(o);
    }
    let mut s = photocraft_engine::Session::new();
    let r = s.execute("file.automate.runDroplet", p).map_err(|e| e.to_string())?;
    for f in r["files"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "ok    {}", f.as_str().unwrap_or_default());
    }
    let errors = r["errors"].as_array().cloned().unwrap_or_default();
    for e in &errors {
        let _ = writeln!(err, "FAIL  {}: {}", e["file"].as_str().unwrap_or_default(), e["error"].as_str().unwrap_or_default());
    }
    if errors.is_empty() { Ok(()) } else { Err(format!("{} file(s) failed", errors.len())) }
}

fn commands(a: &Args, out: &mut dyn Write) -> R {
    let h = Headless::new();
    let list = h.command_list();
    let needle = a.get("--filter").map(str::to_lowercase);
    let items: Vec<&Value> = list
        .as_array()
        .map(|v| v.iter().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            needle.as_ref().is_none_or(|n| format!("{} {}", c["id"].as_str().unwrap_or(""), c["label"].as_str().unwrap_or("")).to_lowercase().contains(n))
        })
        .collect();
    if a.has("--json") {
        return print_json(out, &Value::Array(items.into_iter().cloned().collect()), false);
    }
    for c in items {
        let menu = c["menu"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" > ")).unwrap_or_default();
        writeln!(out, "{:<44} {:<32} {}", c["id"].as_str().unwrap_or(""), c["label"].as_str().unwrap_or(""), menu).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn serve(a: &Args, err: &mut dyn Write) -> R {
    use std::sync::{Arc, Mutex};
    let h = Arc::new(Mutex::new(Headless::with_workspace(automation_workspace(a)?)));
    match a.get("--port") {
        Some(port) => {
            let port: u16 = port.parse().map_err(|_| format!("bad --port `{port}`"))?;
            let addr = format!("127.0.0.1:{port}");
            let (supplied, token_file) = security::token_inputs(a.get("--control-token").map(str::to_owned), a.get("--control-token-file").map(PathBuf::from));
            let token = security::server_token(supplied.as_deref(), token_file.as_deref()).map_err(|e| e.to_string())?;
            if let Some(path) = &token_file {
                let _ = writeln!(err, "photocraft-cli control token file: {}", path.display());
            } else if supplied.is_none() {
                let _ = writeln!(err, "photocraft-cli control token: {token}");
            }
            photocraft_automation::rpc::serve_tcp(&addr, h, token, |local| {
                let _ = writeln!(err, "photocraft-cli serving on {local}");
            })
            .map_err(|e| e.to_string())
        }
        None => {
            let stdin = std::io::stdin();
            photocraft_automation::rpc::serve_lines(&h, stdin.lock(), std::io::stdout()).map_err(|e| e.to_string())
        }
    }
}

fn mcp(a: &Args) -> R {
    let server = match a.get("--bridge") {
        Some(addr) => {
            let (supplied, token_file) = security::token_inputs(a.get("--control-token").map(str::to_owned), a.get("--control-token-file").map(PathBuf::from));
            let token = security::client_token(supplied.as_deref(), token_file.as_deref()).map_err(|e| e.to_string())?;
            PhotocraftMcp::bridge(addr, &token).map_err(|e| e.to_string())?
        }
        None => PhotocraftMcp::headless_with_workspace(automation_workspace(a)?),
    };
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(server.serve_stdio()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod missing_font_warning_tests {
    use super::*;

    fn invoke(args: &[&str]) -> (i32, String, String) {
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run(&args, &mut output, &mut errors);
        (code, String::from_utf8(output).unwrap(), String::from_utf8(errors).unwrap())
    }

    #[test]
    fn missing_typeface_is_reported_by_run_info_and_convert() {
        let family = "DefinitelyMissingPhotoCraftTypeface1251";
        let folder = std::env::temp_dir().join(format!("photocraft-cli-font-warnings-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let native = folder.join("missing.pcraft");
        let png = folder.join("missing.png");
        let native_str = native.to_string_lossy().to_string();
        let png_str = png.to_string_lossy().to_string();
        let params = json!({"x": 6, "y": 38, "text": "Missing font warning", "font": family, "size": 20}).to_string();

        let (code, stdout, stderr) = invoke(&[
            "run",
            "--new",
            r#"{"width":256,"height":96,"background":"white"}"#,
            "--cmd",
            "type.create",
            "--params",
            &params,
            "--cmd",
            "type.resolveMissingFonts",
            "--out",
            &native_str,
        ]);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stderr.matches(family).count(), 1, "multi-command runs should not repeat the same missing font warning");
        let commands: Vec<Value> = stdout.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(commands.len(), 2, "{stdout}");
        assert!(commands[0]["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|s| s.contains(family))));
        assert!(commands[1].get("warnings").is_none(), "only newly detected missing fonts are reported");

        let (code, stdout, stderr) = invoke(&["info", &native_str, "--compact"]);
        assert_eq!(code, 0, "{stderr}");
        let info: Value = serde_json::from_str(&stdout).unwrap();
        assert!(info["warnings"].as_array().unwrap().iter().any(|w| w.as_str().is_some_and(|s| s.contains(family))));
        assert!(stderr.is_empty(), "info puts warnings in JSON, not stderr");

        let (code, _, stderr) = invoke(&["convert", &native_str, &png_str]);
        assert_eq!(code, 0, "{stderr}");
        assert!(stderr.contains(&format!("warning: font '{family}'")), "{stderr}");
        assert!(png.is_file());

        // Even with no commands, run --out checks the opened document before export.
        let (code, stdout, stderr) = invoke(&["run", &native_str, "--out", &png_str]);
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.is_empty());
        assert_eq!(stderr.matches(family).count(), 1);

        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn convert_reads_and_writes_openraster() {
        let folder = std::env::temp_dir().join(format!("photocraft-cli-ora-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = |name: &str| folder.join(name).to_string_lossy().to_string();
        let (native, ora, png) = (path("layers.pcraft"), path("layers.ora"), path("back.png"));
        let (code, _, stderr) = invoke(&["run", "--new", r#"{"width":16,"height":12}"#, "--cmd", "layer.new.layer", "--out", &native]);
        assert_eq!(code, 0, "{stderr}");
        let (code, _, stderr) = invoke(&["convert", &native, &ora]);
        assert_eq!(code, 0, "{stderr}");
        let (code, stdout, stderr) = invoke(&["info", &ora, "--compact"]);
        assert_eq!(code, 0, "{stderr}");
        let info: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(info["layers"].as_array().map(Vec::len), Some(2), "{info}");
        let (code, _, stderr) = invoke(&["convert", &ora, &png]);
        assert_eq!(code, 0, "{stderr}");
        assert!(is_input(std::path::Path::new(&ora)));
        // Krita documents are batch inputs too (import-only).
        for name in ["sketch.kra", "old.KRZ"] {
            std::fs::write(folder.join(name), b"").unwrap();
            assert!(is_input(&folder.join(name)), "{name}");
        }
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn no_type_layers_do_not_produce_missing_font_warnings() {
        let folder = std::env::temp_dir().join(format!("photocraft-cli-no-font-warning-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let native = folder.join("blank.pcraft");
        let png = folder.join("blank.png");
        let native_str = native.to_string_lossy().to_string();
        let png_str = png.to_string_lossy().to_string();
        let (code, _, stderr) = invoke(&["run", "--new", r#"{"width":16,"height":16}"#, "--out", &native_str]);
        assert_eq!(code, 0, "{stderr}");
        assert!(!stderr.contains("font '"), "{stderr}");
        let (code, stdout, stderr) = invoke(&["info", &native_str, "--compact"]);
        assert_eq!(code, 0, "{stderr}");
        let info: Value = serde_json::from_str(&stdout).unwrap();
        assert!(info["warnings"].as_array().unwrap().iter().all(|w| !w.as_str().unwrap_or_default().contains("font '")));
        let (code, _, stderr) = invoke(&["convert", &native_str, &png_str]);
        assert_eq!(code, 0, "{stderr}");
        assert!(!stderr.contains("font '"), "{stderr}");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

#[cfg(test)]
mod batch_action_set_tests {
    use super::*;

    fn invoke(args: &[&str]) -> (i32, String, String) {
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run(&args, &mut output, &mut errors);
        (code, String::from_utf8(output).unwrap(), String::from_utf8(errors).unwrap())
    }

    /// A batched action that plays another action of its set finds it in every file's session; a
    /// failure inside the called action fails the file, and a self-call stops instead of looping.
    #[test]
    fn batch_plays_an_action_that_calls_another_action_of_its_set() {
        let dir = std::env::temp_dir().join(format!("photocraft-cli-action-set-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = |name: &str| dir.join(name).to_string_lossy().to_string();
        std::fs::create_dir_all(dir.join("in")).unwrap();
        for name in ["a", "b"] {
            let (code, _, stderr) = invoke(&["run", "--new", r#"{"width":8,"height":6}"#, "--out", &path(&format!("in/{name}.png"))]);
            assert_eq!(code, 0, "{stderr}");
        }
        let set = json!({"version": 1, "actions": [
            {"name": "Outer", "steps": [["actions.play", {"action": "Rotate"}]]},
            {"name": "Rotate", "steps": [["image.imageRotation.90cw", {}]]},
            {"name": "Calls bad", "steps": [["actions.play", {"action": "Bad"}]]},
            {"name": "Bad", "steps": [["layer.delete", {"layer": 999_999}]]},
            {"name": "Loop", "steps": [["actions.play", {"action": "Loop"}]]},
        ]});
        std::fs::write(dir.join("actions.json"), set.to_string()).unwrap();
        let batch = |action: &str| invoke(&["batch", "--actions", &path("actions.json"), "--action", action, "--in", &path("in"), "--out", &path(action)]);

        let (code, stdout, stderr) = batch("Outer");
        assert_eq!(code, 0, "{stdout}{stderr}");
        for name in ["a", "b"] {
            let (code, info, stderr) = invoke(&["info", &path(&format!("Outer/{name}.png")), "--compact"]);
            assert_eq!(code, 0, "{stderr}");
            let info: Value = serde_json::from_str(&info).unwrap();
            assert_eq!((info["width"].as_u64(), info["height"].as_u64()), (Some(6), Some(8)), "rotated by the called action");
        }
        for action in ["Calls bad", "Loop"] {
            let (code, stdout, stderr) = batch(action);
            assert_ne!(code, 0, "{stdout}");
            assert_eq!(stderr.matches("FAIL").count(), 2, "{stderr}");
            assert!(!dir.join(action).join("a.png").exists(), "a file whose called action failed is not saved");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod store_cli_tests {
    use super::*;

    fn invoke(args: &[&str]) -> (i32, String, String) {
        let (mut output, mut errors) = (Vec::new(), Vec::new());
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run(&args, &mut output, &mut errors);
        (code, String::from_utf8(output).unwrap(), String::from_utf8(errors).unwrap())
    }

    fn scratch(name: &str) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("photocraft-cli-store-{name}-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `store-*` commands round-trip bytes through a local backend, with the same JSON shapes
    /// as wordcraft-storage (`path`/`size`/`etag`, `isDir`, probe caps, recover counts).
    #[test]
    fn store_commands_round_trip_local() {
        let dir = scratch("cmds");
        let root = dir.join("store").to_string_lossy().to_string();
        let src = dir.join("hello.bin");
        std::fs::write(&src, b"8BPS-stage").unwrap();
        let src = src.to_string_lossy().to_string();
        let store: [&str; 4] = ["--store", "local", "--store-root", &root];

        let (code, stdout, stderr) = invoke(&["store-write", "docs/memo.psd", &src, store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let written: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!((written["path"].as_str(), written["size"].as_u64()), (Some("docs/memo.psd"), Some(10)));
        assert!(written["etag"].as_str().is_some_and(|t| !t.is_empty()));

        let (code, stdout, stderr) = invoke(&["store-stat", "docs/memo.psd", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let stat: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(stat["size"].as_u64(), Some(10));
        assert_eq!(stat["etag"], written["etag"]);
        assert_eq!(stat["isDir"], json!(false));

        let (code, stdout, stderr) = invoke(&["store-list", "docs", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let list: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(list.as_array().map(Vec::len), Some(1));
        assert_eq!(list[0]["path"].as_str(), Some("docs/memo.psd"));

        let dst = dir.join("back.bin").to_string_lossy().to_string();
        let (code, _, stderr) = invoke(&["store-read", "docs/memo.psd", &dst, store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        assert_eq!(std::fs::read(&dst).unwrap(), b"8BPS-stage");

        let (code, stdout, stderr) = invoke(&["store-probe", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let probe: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(probe["atomicWrite"], json!(true));

        let (code, stdout, stderr) = invoke(&["store-recover", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let recover: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(recover["removedTemp"], json!(0));

        let (code, _, stderr) = invoke(&["store-remove", "docs/memo.psd", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        // Removing again succeeds (idempotent).
        let (code, _, stderr) = invoke(&["store-remove", "docs/memo.psd", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 0, "{stderr}");
        let (code, _, stderr) = invoke(&["store-stat", "docs/memo.psd", store[0], store[1], store[2], store[3]]);
        assert_eq!(code, 1, "removed file stats as missing: {stderr}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Conditional writes: a stale `--etag` loses, a fresh one wins.
    #[test]
    fn store_write_etag_guards_replacement() {
        let dir = scratch("etag");
        let root = dir.join("store").to_string_lossy().to_string();
        let src = |tag: &str, data: &[u8]| {
            let p = dir.join(format!("v-{tag}.bin"));
            std::fs::write(&p, data).unwrap();
            p.to_string_lossy().to_string()
        };
        let (v1, v2) = (src("a", b"one"), src("b", b"two"));
        let (code, stdout, stderr) = invoke(&["store-write", "f.psd", &v1, "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");
        let tag = serde_json::from_str::<Value>(&stdout).unwrap()["etag"].as_str().unwrap().to_string();

        let (code, _, stderr) = invoke(&["store-write", "f.psd", &v2, "--store", "local", "--store-root", &root, "--etag", "stale"]);
        assert_eq!(code, 1, "{stderr}");
        let (code, _, stderr) = invoke(&["store-write", "f.psd", &v2, "--store", "local", "--store-root", &root, "--etag", &tag, "--atomic"]);
        assert_eq!(code, 0, "{stderr}");
        let (code, stdout, _) = invoke(&["store-list", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0);
        assert!(stdout.contains("f.psd"), "{stdout}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `convert`/`info`/`run` compose with `--store*`: storage paths in, storage paths out.
    #[test]
    fn convert_info_run_compose_with_local_store() {
        let dir = scratch("compose");
        let root = dir.join("store").to_string_lossy().to_string();
        // A real PNG to start from.
        let seed = dir.join("seed.png").to_string_lossy().to_string();
        let (code, _, stderr) = invoke(&["run", "--new", r#"{"width":16,"height":12}"#, "--out", &seed]);
        assert_eq!(code, 0, "{stderr}");

        let (code, _, stderr) = invoke(&["store-write", "in/seed.png", &seed, "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");

        let (code, _, stderr) = invoke(&["convert", "in/seed.png", "out/back.png", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");

        let (code, stdout, stderr) = invoke(&["info", "out/back.png", "--compact", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");
        let info: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!((info["width"].as_u64(), info["height"].as_u64()), (Some(16), Some(12)));
        assert_eq!(info["file"].as_str(), Some("out/back.png"));

        let (code, stdout, stderr) =
            invoke(&["run", "in/seed.png", "--cmd", "image.imageRotation.90cw", "--out", "out/rot.png", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stdout}{stderr}");
        let (code, stdout, stderr) = invoke(&["info", "out/rot.png", "--compact", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");
        let info: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!((info["width"].as_u64(), info["height"].as_u64()), (Some(12), Some(16)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `batch` with `--store*` treats `--in`/`--out` as storage prefixes.
    #[test]
    fn batch_composes_with_local_store() {
        let dir = scratch("batch");
        let root = dir.join("store").to_string_lossy().to_string();
        let seed = dir.join("a.png").to_string_lossy().to_string();
        let (code, _, stderr) = invoke(&["run", "--new", r#"{"width":8,"height":6}"#, "--out", &seed]);
        assert_eq!(code, 0, "{stderr}");
        let (code, _, stderr) = invoke(&["store-write", "bin/a.png", &seed, "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");
        let actions = dir.join("actions.json");
        std::fs::write(&actions, r#"[["image.imageRotation.90cw", {}]]"#).unwrap();
        let actions = actions.to_string_lossy().to_string();

        let (code, stdout, stderr) =
            invoke(&["batch", "--actions", &actions, "--in", "bin", "--out", "bout", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stdout}{stderr}");
        assert!(stdout.contains("1 succeeded"), "{stdout}");

        let (code, stdout, stderr) = invoke(&["store-list", "bout", "--store", "local", "--store-root", &root]);
        assert_eq!(code, 0, "{stderr}");
        assert!(stdout.contains("bout/a.png"), "{stdout}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Flag misuse fails before any network I/O (all unreachable hosts).
    #[test]
    fn store_flag_misuse_is_a_usage_error() {
        let dir = scratch("misuse");
        let root = dir.join("store").to_string_lossy().to_string();

        let (code, _, stderr) = invoke(&["store-probe", "--store", "vault"]);
        assert_eq!(code, 1, "unknown backend is an error, but got {code}: {stderr}");
        assert!(stderr.contains("unknown --store"), "{stderr}");

        let (code, _, stderr) = invoke(&["store-probe"]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("--store-root"), "{stderr}");

        let (code, _, stderr) = invoke(&["store-probe", "--store", "webdav"]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("--store-url"), "{stderr}");

        // Token + Basic never reaches the network.
        let (code, _, stderr) = invoke(&[
            "store-probe",
            "--store",
            "webdav",
            "--store-url",
            "http://127.0.0.1:9/unused/",
            "--store-token",
            "tok",
            "--store-user",
            "u",
        ]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("pick one auth scheme"), "{stderr}");

        // Cross-backend flags are rejected without connecting.
        let (code, _, stderr) =
            invoke(&["store-probe", "--store", "local", "--store-root", &root, "--store-url", "http://127.0.0.1:9/x/"]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("--store-url is for --store webdav"), "{stderr}");

        // https is refused at construction, not after a dial.
        let (code, _, stderr) = invoke(&["store-probe", "--store", "webdav", "--store-url", "https://cloud.example/dav/"]);
        assert_eq!(code, 1, "{stderr}");
        assert!(stderr.contains("http"), "{stderr}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
