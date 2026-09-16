fn main() {
    // exe 资源:应用图标(资源管理器/任务栏/标题栏)。Windows 专属段
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.compile().unwrap();
    }
}
