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
mod cam_sheet;
mod cam_template;
mod cam_tool_import;
mod cam_paths;
mod cam_post;
mod cam_sim;
mod cam_stock;
mod cam_ui;
mod palette;
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
mod fea_mesh;
mod fea_solve;
mod fea_ui;
mod icon_paths;
mod icons;
mod import;
mod kernel;
mod occ;
mod regions;
mod printing;
mod render;
mod script;
mod script_api;
mod script_num;
mod script_ui;
mod sketch;
mod sketch_import;
mod sketcher;
mod text;
mod theme;
mod viewport;
mod web_asm;
mod web_export;
mod worker;
mod workspace;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if matches!(args.get(1).map(|s| s.as_str()), Some("run") | Some("script") | Some("--help") | Some("-h")) {
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
            let Some(path) = args.get(2) else {
                eprintln!("usage: cad-app run script.json");
                std::process::exit(2);
            };
            std::process::exit(agent::run_script(path));
        }
        Some("script") => {
            let Some(path) = args.get(2) else {
                eprintln!("usage: cad-app script file.rhai [args…]");
                std::process::exit(2);
            };
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
        Some("--help") | Some("-h") => {
            println!("cad-app [part.json]        open the editor\ncad-app mcp                Model Context Protocol server on stdio (for AI agents)\ncad-app run script.json    run a JSON list of tool calls (see AGENTS.md)\ncad-app script file.rhai   run a Rhai script with no window (see SCRIPTING.md)");
            return Ok(());
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
        viewport: egui::ViewportBuilder::default().with_inner_size([1600.0, 1000.0]).with_min_inner_size([320.0, 400.0]).with_title("cad-app"),
        ..Default::default()
    };
    eframe::run_native("cad-app", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
