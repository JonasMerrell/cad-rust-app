// No console window behind the editor on Windows; the CLI modes attach to the parent console.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod agent;
mod app;
mod asm_ui;
mod assembly;
mod cam;
mod cam_3d;
mod cam_engrave;
mod cam_paths;
mod cam_post;
mod cam_sim;
mod cam_stock;
mod cam_ui;
mod camera;
mod dialog;
mod doc;
mod drawing;
mod drawing_export;
mod drawing_ui;
mod expr;
mod icon_paths;
mod icons;
mod import;
mod kernel;
mod occ;
mod regions;
mod printing;
mod render;
mod script;
mod script_ui;
mod sketch;
mod sketcher;
mod text;
mod theme;
mod viewport;
mod worker;
mod workspace;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if matches!(args.get(1).map(|s| s.as_str()), Some("run") | Some("--help") | Some("-h")) {
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
        Some("--help") | Some("-h") => {
            println!("cad-app [part.json]        open the editor\ncad-app mcp                Model Context Protocol server on stdio (for AI agents)\ncad-app run script.json    run a JSON list of tool calls (see AGENTS.md)");
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
