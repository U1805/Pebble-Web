// 静态文件服务：阶段七接入前端构建产物时启用。
//
// 计划：由 Config::static_dir 指向构建输出（仓库根 dist/ 或独立 web-dist/），
// 使用 tower-http ServeDir + ServeFile（SPA fallback 到 index.html）。
// 阶段三暂不挂载，避免对不存在的目录产生误导性 404。