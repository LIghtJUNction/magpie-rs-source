//! Render the production plugin-composed shell against isolated synthetic data.
//! No live accounts, keyring writes or supervised services are started.

use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{AppContext, HeadlessAppContext, Result, px, size};
use magpie_desktop::{
    Appearance, Assets, Locale, MagpieWindow, font_for_families, init_material_theme,
};
use serde_json::json;

const GPUI_REVISION: &str = "47895b92558883d24e19d54fdeb876ed551d9394";
const FIXTURE_CONFIG: &str = "model = \"fixture/preview-model\"\n\
    model_provider = \"fixture\"\nmodel_reasoning_effort = \"high\"\n";
const VIEWPORTS: [(u32, u32); 4] = [(900, 560), (360, 560), (440, 560), (1280, 720)];

#[derive(Clone, Copy)]
enum Scene {
    Agents,
    Codex,
    CodexUpdate,
    Providers,
    ProviderEditor,
    ProviderCredentials,
    Settings,
    Gateway,
    GatewayExternal,
    GatewayError,
    Usage,
    Plugins,
    ProviderPresets,
}

impl Scene {
    fn name(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Codex => "codex",
            Self::CodexUpdate => "codex-update",
            Self::Providers => "providers",
            Self::ProviderEditor => "provider-editor",
            Self::ProviderCredentials => "provider-credentials",
            Self::Settings => "settings",
            Self::Gateway => "gateway",
            Self::GatewayExternal => "gateway-external",
            Self::GatewayError => "gateway-error",
            Self::Usage => "usage",
            Self::Plugins => "plugins",
            Self::ProviderPresets => "provider-presets",
        }
    }
}

struct Options {
    output: PathBuf,
    locale: Locale,
    locale_name: String,
    all: bool,
    viewport: Option<(u32, u32)>,
    worker: bool,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn options() -> Result<Option<Options>> {
    let mut output = None;
    let mut locale_name = "zh-CN".to_owned();
    let mut all = false;
    let mut viewport = None;
    let mut worker = false;
    let mut arguments = env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--output") if output.is_none() => {
                output = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| invalid("--output needs a path"))?,
                ));
            }
            Some("--locale") => {
                locale_name = arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or_else(|| invalid("--locale needs a language tag"))?;
            }
            Some("--size") if viewport.is_none() => {
                let value = arguments
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or_else(|| invalid("--size needs WIDTHxHEIGHT"))?;
                let (width, height) = value
                    .split_once('x')
                    .ok_or_else(|| invalid("--size needs WIDTHxHEIGHT"))?;
                let width = width.parse::<u32>().map_err(|_| invalid("invalid width"))?;
                let height = height
                    .parse::<u32>()
                    .map_err(|_| invalid("invalid height"))?;
                if !(240..=1920).contains(&width) || !(240..=1080).contains(&height) {
                    return Err(invalid("--size must be within 240x240 and 1920x1080").into());
                }
                viewport = Some((width, height));
            }
            Some("--all") => all = true,
            Some("--isolated-worker") => worker = true,
            Some("--help" | "-h") => {
                println!(
                    "render_preview --output ABS_NEW_DIRECTORY [--locale en|zh-CN|zh-TW|ja|de] [--all]\n\
                     Default: Agents, light theme, 900x560; one 1x PNG, native texture and manifest.json.\n\
                     --all: 104 captures across 13 scenes, both themes and four viewports.\n\
                     --size: render one custom viewport; combine with --all for 26 scene/theme pairs.\n\
                     Uses an isolated child, real GPUI scene rendering and system fonts."
                );
                return Ok(None);
            }
            _ => return Err(invalid("unknown argument; use --help").into()),
        }
    }
    let locale = match locale_name.as_str() {
        "en" => Locale::English,
        "zh-CN" => Locale::SimplifiedChinese,
        "zh-TW" => Locale::TraditionalChinese,
        "ja" => Locale::Japanese,
        "de" => Locale::German,
        _ => return Err(invalid("unsupported --locale").into()),
    };
    let output = output.ok_or_else(|| invalid("--output is required"))?;
    if !output.is_absolute() {
        return Err(invalid("--output must be absolute").into());
    }
    Ok(Some(Options {
        output,
        locale,
        locale_name,
        all,
        viewport,
        worker,
    }))
}

struct FixtureDirectory(PathBuf);

impl FixtureDirectory {
    fn new() -> Result<Self> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let root = env::temp_dir().join(format!(
            "magpie-render-preview-{}-{nonce}",
            std::process::id()
        ));
        require_external(&root)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root)?;
        let fixture = Self(root);
        for name in [
            "home", "codex", "bin", "config", "cache", "data", "state", "runtime", "tmp",
        ] {
            builder.create(fixture.0.join(name))?;
        }
        fs::write(fixture.0.join("codex/config.toml"), FIXTURE_CONFIG)?;
        use magpie_core::provider_store::{Provider, ProviderProtocol, ProviderStore};
        let path = fixture.0.join("config/providers.toml");
        for (index, (id, name, protocol, base_url)) in [
            (
                "fixture-openai",
                "Fixture · Research models",
                ProviderProtocol::OpenAiResponses,
                "https://models.example.invalid/v1",
            ),
            (
                "fixture-anthropic",
                "Fixture · Writing models",
                ProviderProtocol::AnthropicMessages,
                "https://writing.example.invalid",
            ),
            (
                "fixture-local",
                "Fixture · Local workspace",
                ProviderProtocol::OpenAiChat,
                "http://127.0.0.1:8099/v1",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            ProviderStore::create(
                &path,
                Provider {
                    id: id.into(),
                    name: name.into(),
                    protocol,
                    base_url: base_url.into(),
                    api_key_env: (index < 2).then(|| "SYNTHETIC_PREVIEW_API_KEY".into()),
                    credential_ref: None,
                    models: vec!["fixture-model-a".into(), "fixture-model-b".into()],
                    enabled: index != 1,
                },
                index as u64,
            )?;
        }
        Ok(fixture)
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("render_preview: fixture cleanup failed: {error}");
        }
    }
}

fn require_external(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("directory needs a parent"))?
        .canonicalize()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let recipe = source.parent().and_then(Path::parent);
    if parent.starts_with(&source)
        || recipe
            .is_some_and(|root| root.join("recipes/magpie").is_dir() && parent.starts_with(root))
    {
        return Err(invalid("preview outputs and fixtures must be outside both checkouts").into());
    }
    if path.file_name().is_none() {
        return Err(invalid("directory needs a final component").into());
    }
    Ok(())
}

fn isolated(options: &Options) -> Result<ExitCode> {
    require_external(&options.output)?;
    let fixture = FixtureDirectory::new()?;
    // Re-exec avoids unsafe process-global environment mutation in Rust 2024.
    let mut command = Command::new(env::current_exe()?);
    command
        .args(["--isolated-worker", "--output"])
        .arg(&options.output)
        .args(["--locale", &options.locale_name])
        .env_clear()
        .env("MAGPIE_RENDER_FIXTURE_ROOT", &fixture.0)
        .env("USERPROFILE", fixture.0.join("home"))
        .env("MAGPIE_CONFIG_DIR", fixture.0.join("config"))
        .env("XDG_CONFIG_HOME", fixture.0.join("config"))
        .env("XDG_CACHE_HOME", fixture.0.join("cache"))
        .env("XDG_DATA_HOME", fixture.0.join("data"))
        .env("XDG_STATE_HOME", fixture.0.join("state"))
        .env("XDG_RUNTIME_DIR", fixture.0.join("runtime"))
        .env("PATH", fixture.0.join("bin"))
        .env("TMPDIR", fixture.0.join("tmp"))
        .env("TMP", fixture.0.join("tmp"))
        .env("TEMP", fixture.0.join("tmp"))
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .current_dir(&fixture.0);
    if options.all {
        command.arg("--all");
    }
    if let Some((width, height)) = options.viewport {
        command.args(["--size", &format!("{width}x{height}")]);
    }
    for name in [
        "WGPU_BACKEND",
        "WGPU_ADAPTER_NAME",
        "WGPU_POWER_PREF",
        "WGPU_FORCE_FALLBACK_ADAPTER",
        "VK_ICD_FILENAMES",
        "VK_DRIVER_FILES",
        "LIBGL_ALWAYS_SOFTWARE",
        "MESA_LOADER_DRIVER_OVERRIDE",
        "DRI_PRIME",
    ] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = command.status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn fixture_root() -> Result<PathBuf> {
    let root = env::var_os("MAGPIE_RENDER_FIXTURE_ROOT")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("worker requires the isolated launcher"))?
        .canonicalize()?;
    for (name, relative) in [
        ("PATH", "bin"),
        ("MAGPIE_CONFIG_DIR", "config"),
        ("XDG_CONFIG_HOME", "config"),
    ] {
        if env::var_os(name).map(PathBuf::from).as_deref() != Some(root.join(relative).as_path()) {
            return Err(invalid(format!("worker has an unisolated {name}")).into());
        }
    }
    if fs::read_dir(root.join("bin"))?.next().is_some()
        || fs::read(root.join("codex/config.toml"))? != FIXTURE_CONFIG.as_bytes()
    {
        return Err(invalid("worker fixture changed").into());
    }
    Ok(root)
}

fn render(options: &Options) -> Result<()> {
    let fixture = fixture_root()?;
    require_external(&options.output)?;
    fs::create_dir(&options.output)?;
    let output = options.output.canonicalize()?;
    let platform = gpui::platform::current_platform(true);
    let backend = platform.text_system();
    let families = backend.all_font_names();
    let selected_font = font_for_families(&families, options.locale);
    let mut cx = HeadlessAppContext::with_platform(
        backend,
        Arc::new(Assets),
        gpui::platform::current_headless_renderer,
    );
    cx.update(|cx| {
        gpui::init(cx);
        gpui_kit_material::init(cx);
        init_material_theme(None, cx);
    });
    let viewports: &[(u32, u32)] = match &options.viewport {
        Some(viewport) => std::slice::from_ref(viewport),
        None if options.all => &VIEWPORTS,
        None => &[(900, 560)],
    };
    let themes: &[(&str, Appearance)] = if options.all {
        &[("light", Appearance::Light), ("dark", Appearance::Dark)]
    } else {
        &[("light", Appearance::Light)]
    };
    let scenes: &[Scene] = if options.all {
        &[
            Scene::Agents,
            Scene::Codex,
            Scene::CodexUpdate,
            Scene::Providers,
            Scene::ProviderEditor,
            Scene::ProviderCredentials,
            Scene::Settings,
            Scene::Gateway,
            Scene::GatewayExternal,
            Scene::GatewayError,
            Scene::Usage,
            Scene::Plugins,
            Scene::ProviderPresets,
        ]
    } else {
        &[Scene::Agents]
    };
    let mut records = Vec::new();
    for &(width, height) in viewports {
        for &(theme_name, appearance) in themes {
            let handle =
                cx.open_window(size(px(width as f32), px(height as f32)), |window, cx| {
                    cx.new(|cx| {
                        gpui::base::Root::new(cx.new(MagpieWindow::new_for_preview), window, cx)
                    })
                })?;
            // Production discovery sees only the empty synthetic PATH and HOME.
            cx.run_until_parked();
            for &scene in scenes {
                handle.update(&mut cx, |view, window, cx| {
                    view.view()
                        .clone()
                        .downcast::<MagpieWindow>()
                        .expect("preview root contains MagpieWindow")
                        .update(cx, |view, cx| {
                            view.preview_fixture(
                                &fixture,
                                scene.name(),
                                options.locale.id(),
                                appearance == Appearance::Dark,
                                cx,
                            );
                            view.focus(window, cx);
                        });
                    window.refresh();
                })?;
                // Drain asset loads and draw actual production elements, including text and SVGs.
                for _ in 0..4 {
                    cx.advance_clock(std::time::Duration::from_millis(250));
                    for _ in 0..3 {
                        cx.advance_clock(std::time::Duration::from_millis(250));
                        cx.run_until_parked();
                        cx.update_window(handle.into(), |_, window, cx| {
                            window.refresh();
                            window.draw(cx).clear(cx)
                        })?;
                    }
                }
                if matches!(scene, Scene::ProviderCredentials) {
                    handle.update(&mut cx, |view, window, cx| {
                        view.view()
                            .clone()
                            .downcast::<MagpieWindow>()
                            .expect("preview root contains MagpieWindow")
                            .update(cx, |view, cx| view.preview_reveal_credentials(window, cx));
                        window.refresh();
                    })?;
                    cx.run_until_parked();
                    cx.update_window(handle.into(), |_, window, cx| window.draw(cx).clear(cx))?;
                }
                let mut image = cx.capture_screenshot(handle.into())?;
                let (native_width, native_height) = image.dimensions();
                let density = native_width / width;
                if !(1..=4).contains(&density)
                    || native_width != width * density
                    || native_height != height * density
                {
                    return Err(invalid("unexpected headless texture dimensions").into());
                }
                let stem = format!(
                    "{}-{}-{theme_name}-{width}x{height}",
                    scene.name(),
                    options.locale_name
                );
                let native_name = format!("{stem}-native.png");
                image.save(output.join(&native_name))?;
                // TestPlatform defaults to 2x. Preserve that raw texture, then average
                // its integer-density RGBA blocks for an explicitly labelled 1x preview.
                // The returned image's public flat-sample API needs no extra dependency.
                if density != 1 {
                    let mut samples = image.clone().into_flat_samples();
                    samples.samples.clear();
                    for y in 0..height {
                        for x in 0..width {
                            let mut channels = [0_u32; 4];
                            for dy in 0..density {
                                for dx in 0..density {
                                    let pixel = image.get_pixel(x * density + dx, y * density + dy);
                                    for (sum, value) in channels.iter_mut().zip(pixel.0) {
                                        *sum += u32::from(value);
                                    }
                                }
                            }
                            samples
                                .samples
                                .extend(channels.map(|sum| (sum / (density * density)) as u8));
                        }
                    }
                    samples.layout.width = width;
                    samples.layout.height = height;
                    samples.layout.height_stride = width as usize * 4;
                    image = samples.try_into_buffer().map_err(|(error, _)| {
                        invalid(format!("normalizing preview pixels: {error}"))
                    })?;
                }
                let filename = format!("{stem}.png");
                image.save(output.join(&filename))?;
                // Announce each usable capture before any later matrix scene can fail.
                println!("{}", output.join(&filename).display());
                records.push(json!({
                    "scene": scene.name(), "locale": options.locale_name, "theme": theme_name,
                    "logical_width": width, "logical_height": height,
                    "png": filename, "png_width": image.width(), "png_height": image.height(),
                    "native_png": native_name, "native_width": native_width,
                    "native_height": native_height, "native_density": density,
                    "normalization": "integer-density RGBA box mean; native texture retained",
                }));
            }
            cx.update_window(handle.into(), |_, window, _| window.remove_window())?;
            cx.run_until_parked();
        }
    }
    fs::write(
        output.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1, "gpui_revision": GPUI_REVISION,
            "mode": if options.all { "all" } else { "baseline" },
            "render_path": "HeadlessAppContext + native platform text system + current_headless_renderer",
            "ui_source": "magpie_desktop::shell; production plugin-composed MagpieWindow Render implementation",
            "data": "registered Agent adapter fixtures; three synthetic Providers in an isolated real ProviderStore; runtime plugin catalog and provider presets; synthetic password input, Gateway states and request metadata; no keyring writes, accounts, Agent execution or supervised services launched by this renderer",
            "fonts": families, "primary_font": selected_font.family.as_ref(),
            "fallback_fonts": selected_font.fallbacks.as_ref().map(|fonts| fonts.fallback_list()),
            "ui_source_bytes": include_bytes!("../src/shell.rs").len(),
            "records": records,
            "limitations": [
                "Offscreen scene/text/SVG raster evidence; no OS screen capture or desktop input.",
                "GPU or software-adapter availability and installed fonts affect raster output.",
                "PNG creation is not visual approval, workflow acceptance or original feature parity.",
                "Captures the implemented Agents/Codex, Providers and Settings pages. Other upstream pages remain explicitly unimplemented.",
                "No original visual baseline or cross-platform execution is attested."
            ]
        }))?,
    )?;
    Ok(())
}

fn main() -> ExitCode {
    let result = (|| -> Result<ExitCode> {
        let Some(options) = options()? else {
            return Ok(ExitCode::SUCCESS);
        };
        if options.worker {
            render(&options)?;
            Ok(ExitCode::SUCCESS)
        } else {
            isolated(&options)
        }
    })();
    match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("render_preview: {error:#}");
            ExitCode::FAILURE
        }
    }
}
