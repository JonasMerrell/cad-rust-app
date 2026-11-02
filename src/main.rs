// No console window behind the editor on Windows; the CLI modes attach to the parent console.
#![cfg_attr(windows, windows_subsystem = "windows")]
#![recursion_limit = "256"]

mod agent;
mod app;
mod asm_ui;
mod assembly;
mod cam;
mod cam_3d;
mod cam_engrave;
mod cam_jaws;
mod cam_machine;
mod cam_machine_ui;
mod cam_sheet;
mod cam_template;
mod cam_tool_import;
mod cam_paths;
mod cam_post;
mod cam_sim;
mod cam_stock;
mod cam_ui;
mod palette;
mod parasolid;
mod camera;
mod claude;
mod claude_ui;
mod dialog;
mod doc;
mod drawing;
mod drawing_export;
mod drawing_ui;
mod expr;
mod fea;
mod fea_adv;
mod fea_mesh;
mod fea_solve;
mod fea_ui;
mod circuit;
mod circuit_mcu;
mod circuit_mech;
mod circuit_plot;
mod circuit_solve;
mod circuit_ui;
mod mcu_cm0;
mod mcu_rom;
mod mcu_rp2040;
mod pcb;
mod pcb_export;
mod pcb_lib;
mod pcb_route;
mod pcb_ui;
mod grabcad;
mod home;
mod icon_paths;
mod icons;
mod import;
mod kernel;
mod occ;
mod regions;
mod printing;
mod project;
mod rcnc;
mod render;
mod script;
mod script_api;
mod script_num;
mod script_ui;
mod sketch;
mod sketch_import;
mod sketch_infer;
mod session;
mod sketcher;
mod stdparts;
mod stock_size;
mod stdparts_ui;
mod text;
mod theme;
mod units_switch;
mod vcs;
mod vcs_ui;
mod viewport;
mod web_asm;
mod web_export;
mod worker;
mod workspace;

/// Command line help; `prog` is the name the program was started as (normally rcad).
fn usage(prog: &str) -> String {
    format!(
        "{prog} {}\n\nusage:\n  {prog} [part.json]                         open the editor\n  {prog} mcp                                 Model Context Protocol server on stdio (for AI agents)\n  {prog} run [--stop-on-error] script.json   run a JSON list of tool calls (see AGENTS.md)\n  {prog} script file.rhai [args…]            run a Rhai script with no window (see SCRIPTING.md)\n  {prog} --version | --help",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let prog = args.first().and_then(|a| std::path::Path::new(a).file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "rcad".into());
    #[cfg(windows)]
    if matches!(args.get(1).map(|s| s.as_str()), Some("run") | Some("script") | Some("--help") | Some("-h") | Some("--version") | Some("-V")) || args.get(1).map(|a| a.starts_with('-') && a != "--kernel-worker").unwrap_or(false) {
        // print to the terminal that started us (the MCP server talks over pipes and needs no console)
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
        }
    }
    match args.get(1).map(|s| s.as_str()) {
        Some("mcp") => {
            agent::mcp_main();
            return Ok(());
        }
        Some("run") => {
            let stop = args[2..].iter().any(|a| a == "--stop-on-error");
            let rest: Vec<&String> = args[2..].iter().filter(|a| *a != "--stop-on-error").collect();
            let (Some(path), 1) = (rest.first(), rest.len()) else {
                eprintln!("usage: {prog} run [--stop-on-error] script.json");
                std::process::exit(2);
            };
            if path.starts_with('-') {
                eprintln!("unknown option {path}\nusage: {prog} run [--stop-on-error] script.json");
                std::process::exit(2);
            }
            std::process::exit(agent::run_script(path, stop));
        }
        Some("script") => {
            let Some(path) = args.get(2) else {
                eprintln!("usage: {prog} script file.rhai [args…]");
                std::process::exit(2);
            };
            // stdout carries the script's prints and result: keep OpenCascade's chatter off it
            opencascade_sys::extra::ex_messages_to_stderr();
            match script_api::run_file(path, &args[3..]) {
                Ok(v) => {
                    if !v.is_null() {
                        println!("{}", serde_json::to_string(&v).unwrap_or_default());
                    }
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
        Some("--help") | Some("-h") | Some("help") => {
            println!("{}", usage(&prog));
            return Ok(());
        }
        Some("--version") | Some("-V") | Some("version") => {
            println!("{prog} {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--kernel-worker") => {}
        // (the machine importer reads STEP assemblies in a child process, see cam_machine::import_step)
        Some("--step-parts") if args.len() == 4 => std::process::exit(cam_machine::step_parts_main(&args[2], &args[3])),
        // (-psn_… is what older macOS Finder passes)
        Some(a) if a.starts_with('-') && !a.starts_with("-psn") => {
            eprintln!("unknown option {a}\n\n{}", usage(&prog));
            std::process::exit(2);
        }
        Some(a) if args.len() > 2 => {
            eprintln!("unexpected arguments after {a}\n\n{}", usage(&prog));
            std::process::exit(2);
        }
        _ => {}
    }
    if std::env::args().any(|a| a == "--kernel-worker") {
        worker::worker_main();
        return Ok(());
    }
    env_logger::init();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: render::MSAA as u16,
        depth_buffer: 32,
        // the window opens at the size it had last time
        viewport: egui::ViewportBuilder::default().with_inner_size(session::load_prefs().window.unwrap_or([1600.0, 1000.0])).with_min_inner_size([320.0, 400.0]).with_title("rCAD"),
        ..Default::default()
    };
    eframe::run_native("rCAD", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
