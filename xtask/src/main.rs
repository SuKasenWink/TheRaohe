use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

const TARGET: &str = "aarch64-linux-android";
const BIN: &str = "theraohe";

#[derive(Parser)]
#[command(name = "xtask", about = "TheRaohe 建置 / 部署")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 交叉編譯 aarch64-linux-android 二進位
    Build {
        /// Android API level
        #[arg(long, default_value = "34")]
        api: u32,
        /// NDK 路徑; 預設依序查 ANDROID_NDK_HOME / ANDROID_NDK_ROOT / SDK 的 ndk 目錄
        #[arg(long)]
        ndk: Option<PathBuf>,
        /// Release 建置
        #[arg(long)]
        release: bool,
    },
    /// 打包 tar.gz (777) 到 target/dist, 不經 adb
    Pack,
    /// 打包 tar.gz (777) -> adb push -> 覆蓋解壓 -> 執行
    Deploy {
        /// 多裝置時指定序號
        #[arg(long)]
        serial: Option<String>,
        #[arg(long, default_value = "/data/local/tmp")]
        remote_dir: String,
        #[arg(long, default_value = "--show")]
        run_args: String,
        /// 跳過建置, 直接用現有 release 二進位
        #[arg(long)]
        skip_build: bool,
        /// 先 pkill theraohe (連帶 theraohe-dbg 之類子字串)
        #[arg(long)]
        restart: bool,
        /// setenforce 0
        #[arg(long)]
        permissive: bool,
        /// 背景執行 (spawn 後立即返回)
        #[arg(long)]
        background: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Build { api, ndk, release } => build(api, ndk.as_deref(), release).map(|_| ()),
        Cmd::Pack => {
            let root = repo_root();
            build(34, None, true)?;
            let archive = pack(&root)?;
            let size = fs::metadata(&archive)?.len();
            println!("packed: {}", archive.display());
            println!("size:   {size} bytes");
            Ok(())
        }
        Cmd::Deploy {
            serial,
            remote_dir,
            run_args,
            skip_build,
            restart,
            permissive,
            background,
        } => deploy(
            serial.as_deref(),
            &remote_dir,
            &run_args,
            skip_build,
            restart,
            permissive,
            background,
        ),
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask 固定位於 repo 根目錄下的 xtask/")
        .to_path_buf()
}

fn ndk_host_dir() -> Result<&'static str> {
    match std::env::consts::OS {
        "windows" => Ok("windows-x86_64"),
        "linux" => Ok("linux-x86_64"),
        "macos" => Ok(if std::env::consts::ARCH == "aarch64" {
            "darwin-arm64"
        } else {
            "darwin-x86_64"
        }),
        other => bail!("不支援的宿主平臺: {other}"),
    }
}

fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn version_key(name: &str) -> Vec<u64> {
    name.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect::<String>()
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect()
}

fn resolve_ndk(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_dir() {
            return Ok(p.to_path_buf());
        }
        bail!("找不到 NDK: {}", p.display());
    }
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if p.is_dir() {
                return Ok(p);
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(v) = std::env::var_os(var) {
            roots.push(PathBuf::from(v));
        }
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(v).join("Android").join("Sdk"));
    }
    roots.push(PathBuf::from(r"C:\Android\Sdk"));

    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for root in roots {
        let Ok(entries) = fs::read_dir(root.join("ndk")) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ty) = entry.file_type() else { continue };
            if !ty.is_dir() {
                continue;
            }
            let ver = version_key(&entry.file_name().to_string_lossy());
            if best.as_ref().map(|(v, _)| ver > *v).unwrap_or(true) {
                best = Some((ver, entry.path()));
            }
        }
    }
    best.map(|(_, p)| p)
        .context("找不到 NDK; 請設定 ANDROID_NDK_HOME 或用 --ndk 指定")
}

fn build(api: u32, ndk: Option<&Path>, release: bool) -> Result<PathBuf> {
    let root = repo_root();
    let ndk = resolve_ndk(ndk)?;
    let host = ndk_host_dir()?;
    let bin_dir = ndk
        .join("toolchains")
        .join("llvm")
        .join("prebuilt")
        .join(host)
        .join("bin");
    let clang = bin_dir.join(exe_name("clang"));
    if !clang.is_file() {
        bail!("找不到 clang: {}", clang.display());
    }

    let rustflags = format!(
        "-Clink-arg=--target=aarch64-linux-android{api} -Clink-arg=-fuse-ld=lld -Clink-arg=-Wl,-z,max-page-size=16384"
    );
    let mut cmd = Command::new("cargo");
    cmd.arg("build")
        .arg("--target")
        .arg(TARGET)
        .env("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", &clang)
        .env("CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS", rustflags)
        .env("CC_aarch64_linux_android", &clang)
        .env("AR_aarch64_linux_android", bin_dir.join(exe_name("llvm-ar")))
        .current_dir(&root);
    if release {
        cmd.arg("--release");
    }

    println!("NDK:    {}", ndk.display());
    println!("clang:  {}", clang.display());
    println!("API:    {api}");

    let status = cmd.status().context("執行 cargo build 失敗")?;
    if !status.success() {
        bail!("cargo build 失敗 ({status})");
    }

    let profile = if release { "release" } else { "debug" };
    let out = root.join("target").join(TARGET).join(profile).join(BIN);
    if out.is_file() {
        let size = fs::metadata(&out)?.len();
        println!("built:  {}", out.display());
        println!("size:   {size} bytes");
    }
    Ok(out)
}

/// 777 直接寫進 tar header: 契約在打包這一刻成立, 解壓端不必再 chmod
/// (toybox tar 以 root 解壓會沿用 archive 裡的權限)
fn pack(root: &Path) -> Result<PathBuf> {
    let bin = root.join("target").join(TARGET).join("release").join(BIN);
    if !bin.is_file() {
        bail!("release 二進位不存在: {}", bin.display());
    }
    let out_dir = root.join("target").join("dist");
    fs::create_dir_all(&out_dir)?;
    let archive = out_dir.join(format!("{BIN}.tar.gz"));

    let gz = flate2::write::GzEncoder::new(
        BufWriter::new(File::create(&archive).context("建立 tar.gz 失敗")?),
        flate2::Compression::default(),
    );
    let mut builder = tar::Builder::new(gz);

    let meta = fs::metadata(&bin)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 目錄 entry 也寫 777: 契約在打包這一刻成立, toybox tar 解壓會沿用
    let mut dir_header = tar::Header::new_ustar();
    dir_header.set_entry_type(tar::EntryType::Directory);
    dir_header.set_size(0);
    dir_header.set_mode(0o777);
    dir_header.set_uid(0);
    dir_header.set_gid(0);
    dir_header.set_username("root")?;
    dir_header.set_groupname("root")?;
    dir_header.set_mtime(mtime);
    dir_header.set_cksum();
    builder.append_data(&mut dir_header, format!("{BIN}/"), std::io::empty())?;

    // CopyToRelease/ 下所有內容原樣帶上: (來源, 壓縮包內路徑, 大小, mtime, 是目錄); 皆 777
    let bundle = root.join("CopyToRelease");
    if !bundle.is_dir() {
        bail!("找不到隨行目錄: {}", bundle.display());
    }
    let mut entries: Vec<(PathBuf, String, u64, u64, bool)> = Vec::new();
    entries.push((
        bin.to_path_buf(),
        format!("{BIN}/{BIN}"),
        meta.len(),
        mtime,
        false,
    ));
    collect_bundle(&bundle, &format!("{BIN}/"), &mut entries)?;
    entries.sort_by(|a, b| a.1.cmp(&b.1));

    for (src, name, size, mtime, is_dir) in entries {
        let mut header = tar::Header::new_ustar();
        if is_dir {
            header.set_entry_type(tar::EntryType::Directory);
        }
        header.set_size(size);
        header.set_mode(0o777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_username("root")?;
        header.set_groupname("root")?;
        header.set_mtime(mtime);
        header.set_cksum();
        if is_dir {
            builder.append_data(&mut header, name, std::io::empty())?;
        } else {
            let file = BufReader::new(File::open(src)?);
            builder.append_data(&mut header, name, file)?;
        }
    }

    builder.into_inner()?.finish()?;
    Ok(archive)
}

fn collect_bundle(
    dir: &Path,
    prefix: &str,
    out: &mut Vec<(PathBuf, String, u64, u64, bool)>,
) -> Result<()> {
    let mtime_of = |meta: &fs::Metadata| {
        meta.modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        let meta = fs::metadata(&path)?;
        if path.is_dir() {
            let rel_dir = format!("{rel}/");
            out.push((
                path.clone(),
                rel_dir.clone(),
                0,
                mtime_of(&meta),
                true,
            ));
            collect_bundle(&path, &rel_dir, out)?;
        } else {
            out.push((path, rel, meta.len(), mtime_of(&meta), false));
        }
    }
    Ok(())
}

fn resolve_adb() -> Result<PathBuf> {
    if let Some(p) = which_on_path("adb") {
        return Ok(p);
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME"] {
        if let Some(v) = std::env::var_os(var) {
            roots.push(PathBuf::from(v));
        }
    }
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(v).join("Android").join("Sdk"));
    }
    roots.push(PathBuf::from(r"C:\Android\Sdk"));

    for root in roots {
        let p = root.join("platform-tools").join(exe_name("adb"));
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!("找不到 adb; 請安裝 platform-tools 或加入 PATH")
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&std::env::var_os("PATH")?) {
        for ext in exts {
            let p = dir.join(format!("{name}{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn adb_command(adb: &Path, serial: Option<&str>, args: &[&str]) -> Command {
    let mut cmd = Command::new(adb);
    if let Some(s) = serial {
        cmd.arg("-s").arg(s);
    }
    cmd.args(args);
    cmd
}

fn adb_capture(adb: &Path, serial: Option<&str>, args: &[&str]) -> Result<std::process::Output> {
    adb_command(adb, serial, args)
        .output()
        .with_context(|| format!("執行 adb {} 失敗", args.join(" ")))
}

fn adb_run(adb: &Path, serial: Option<&str>, args: &[&str]) -> Result<ExitStatus> {
    adb_command(adb, serial, args)
        .status()
        .with_context(|| format!("執行 adb {} 失敗", args.join(" ")))
}

fn check_single_device(serial: Option<&str>) -> Result<()> {
    let adb = resolve_adb()?;
    let out = adb_capture(&adb, None, &["devices"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    let ready: Vec<&str> = text
        .lines()
        .skip(1)
        .filter(|l| l.trim_end().ends_with("\tdevice"))
        .collect();
    if ready.is_empty() {
        bail!("沒有已授權的裝置 (adb devices)");
    }
    if ready.len() > 1 && serial.is_none() {
        bail!("找到多個裝置; 用 --serial 指定");
    }
    Ok(())
}

fn is_running(adb: &Path, serial: Option<&str>) -> bool {
    adb_capture(adb, serial, &["shell", "su", "-c", "pidof theraohe"])
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

fn assert_root(adb: &Path, serial: Option<&str>) -> Result<()> {
    let out = adb_capture(adb, serial, &["shell", "su", "-c", "id"])?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !text.contains("uid=0") {
        bail!("su 未取得 root; 請先在 KernelSU/Magisk 對 Shell 授權 root");
    }
    Ok(())
}

fn deploy(
    serial: Option<&str>,
    remote_dir: &str,
    run_args: &str,
    skip_build: bool,
    restart: bool,
    permissive: bool,
    background: bool,
) -> Result<()> {
    let root = repo_root();
    if !skip_build {
        build(34, None, true)?;
    }
    let archive = pack(&root)?;

    let adb = resolve_adb()?;
    check_single_device(serial)?;
    assert_root(&adb, serial)?;

    let dir = remote_dir.trim_end_matches('/');
    let remote_tgz = format!("{dir}/{BIN}.tar.gz");
    let remote_bin = format!("{dir}/{BIN}/{BIN}");

    if restart {
        // 連帶殺掉 theraohe-dbg 這類子字串比對得到的除錯實例
        let _ = adb_run(&adb, serial, &["shell", "su", "-c", "pkill theraohe"]);
    }

    let status = adb_run(&adb, serial, &["push", &archive.to_string_lossy(), &remote_tgz])?;
    if !status.success() {
        bail!("adb push 失敗 ({status})");
    }

    let extract = format!("tar -xzf {remote_tgz} -C {dir}");
    let status = adb_run(&adb, serial, &["shell", "su", "-c", &extract])?;
    if !status.success() {
        // 回退路徑: 疑似舊實例佔用二進位才 pkill 重試; 其它原因 (磁碟滿等) 不動裝置上的行程
        if !is_running(&adb, serial) {
            bail!("解壓失敗 ({status})");
        }
        println!("解壓失敗 ({status}), theraohe 行程存在, pkill 後重試");
        // 連帶殺掉 theraohe-dbg 這類子字串比對得到的除錯實例
        let _ = adb_run(&adb, serial, &["shell", "su", "-c", "pkill theraohe"]);
        let status = adb_run(&adb, serial, &["shell", "su", "-c", &extract])?;
        if !status.success() {
            bail!("解壓失敗 ({status}); pkill 重試後仍失敗");
        }
    }
    let _ = adb_run(&adb, serial, &["shell", "su", "-c", &format!("rm -f {remote_tgz}")]);

    if permissive {
        let status = adb_run(&adb, serial, &["shell", "su", "-c", "setenforce 0"])?;
        if !status.success() {
            bail!("setenforce 0 失敗 ({status})");
        }
        println!("SELinux: permissive");
    }

    let cmd_line = if run_args.trim().is_empty() {
        remote_bin.clone()
    } else {
        format!("{remote_bin} {run_args}")
    };
    println!("run:    {cmd_line}");

    let shell_args: &[&str] = &["shell", "su", "-c", &cmd_line];
    if background {
        // 裝置端 detach: 單引號把 nohup 指令整體交給 su -c, 且 su -c 內 & 讓裝置 sh
        // 立即返回, adb shell 隨之退出, 呼叫方不會被裝置上行程挾持; log 寫 {dir}/{BIN}.log
        // KernelSU 授權是時靈時不靈的, 外層 for 循環對 su 本身重試, 直到授權成功
        let log = format!("{dir}/{BIN}.log");
        let inner = format!("nohup {cmd_line} </dev/null >{log} 2>&1 &");
        let detached = format!(
            "for i in $(seq 60); do su -c '{inner}' >/dev/null 2>&1 && break; sleep 0.5; done"
        );
        let status = adb_run(&adb, serial, &["shell", &detached])?;
        if !status.success() {
            bail!("背景啟動失敗 ({status})");
        }
        // 確認行程就位並把 pid 交給呼叫方, 作為完成回調 (KSU 授權成功到行程註冊有延遲, 輪詢)
        let mut pid = String::new();
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let out = adb_capture(&adb, serial, &["shell", "su", "-c", &format!("pidof {BIN}")])?;
            pid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !pid.is_empty() {
                break;
            }
        }
        if pid.is_empty() {
            bail!("背景啟動後 pidof {BIN} 為空; 查看 {log} 取得失敗原因");
        }
        println!("launched (detach), pid {pid}, log: {log}");
        Ok(())
    } else {
        let status = adb_run(&adb, serial, shell_args)?;
        std::process::exit(status.code().unwrap_or(1));
    }
}
