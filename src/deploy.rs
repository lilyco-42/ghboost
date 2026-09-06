use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// 支持的协议类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Protocol {
    #[serde(rename = "vless-reality")]
    VlessReality,
    #[serde(rename = "vless-ws")]
    VlessWs,
    #[serde(rename = "trojan")]
    Trojan,
    #[serde(rename = "shadowsocks")]
    Shadowsocks,
    #[serde(rename = "hysteria2")]
    Hysteria2,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::VlessReality => write!(f, "vless-reality"),
            Protocol::VlessWs => write!(f, "vless-ws"),
            Protocol::Trojan => write!(f, "trojan"),
            Protocol::Shadowsocks => write!(f, "shadowsocks"),
            Protocol::Hysteria2 => write!(f, "hysteria2"),
        }
    }
}

/// 服务器部署参数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployParams {
    /// 服务器 IP 地址
    pub host: String,
    /// SSH 端口
    pub port: u16,
    /// SSH 用户名
    pub user: String,
    /// SSH 密码（可选，优先使用密钥）
    pub password: Option<String>,
    /// SSH 私钥路径（可选）
    pub key_path: Option<PathBuf>,
    /// 协议类型
    pub protocol: Protocol,
    /// 服务端口（不填则自动分配）
    pub port_out: Option<u16>,
    /// 域名（Reality/TLS 需要）
    pub domain: Option<String>,
    /// 是否安装 BBR 加速
    #[serde(default = "default_true")]
    pub install_bbr: bool,
    /// 是否配置防火墙
    #[serde(default = "default_true")]
    pub configure_firewall: bool,
}

fn default_true() -> bool {
    true
}

/// 部署结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployResult {
    /// 是否成功
    pub success: bool,
    /// 协议
    pub protocol: String,
    /// 服务器地址
    pub server: String,
    /// 端口
    pub port: u16,
    /// 连接 URI（可直接导入客户端）
    pub uri: String,
    /// 配置 JSON（客户端配置）
    pub config_json: serde_json::Value,
    /// 部署日志
    pub logs: Vec<String>,
    /// 错误信息（如果失败）
    pub error: Option<String>,
}

/// 生成 UUID
fn generate_uuid() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.gen();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3],
        bytes[4], bytes[5],
        bytes[6], bytes[7],
        bytes[8], bytes[9],
        bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

/// 生成随机密码
fn generate_password(length: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect()
}

/// SSH 执行命令
fn ssh_exec(
    host: &str,
    port: u16,
    user: &str,
    password: Option<&str>,
    key_path: Option<&PathBuf>,
    cmd: &str,
) -> Result<String, String> {
    let ssh_host = format!("{}@{}", user, host);
    let port_str = port.to_string();
    let key_str;
    let mut args: Vec<&str> = vec![
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "ConnectTimeout=10",
        "-p",
        &port_str,
    ];

    if let Some(key) = key_path {
        key_str = key.to_string_lossy().to_string();
        args.extend_from_slice(&["-i", &key_str]);
    }

    args.push(&ssh_host);
    args.push(cmd);

    let output = if let Some(pwd) = password {
        let mut sshpass_args: Vec<&str> = vec!["-p", pwd, "ssh"];
        sshpass_args.extend_from_slice(&args);
        Command::new("sshpass")
            .args(&sshpass_args)
            .output()
            .map_err(|e| {
                format!(
                    "sshpass 执行失败: {}。请安装 sshpass: apt-get install sshpass",
                    e
                )
            })?
    } else {
        Command::new("ssh")
            .args(&args)
            .output()
            .map_err(|e| format!("ssh 执行失败: {}", e))?
    };

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(format!("命令执行失败: {}", stderr))
    }
}

/// 上传文件到服务器
fn scp_upload(
    host: &str,
    port: u16,
    user: &str,
    password: Option<&str>,
    key_path: Option<&PathBuf>,
    local_path: &PathBuf,
    remote_path: &str,
) -> Result<(), String> {
    let remote = format!("{}@{}:{}", user, host, remote_path);
    let port_str = port.to_string();
    let key_str;
    let local_str;
    let mut args: Vec<&str> = vec![
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-P",
        &port_str,
    ];

    if let Some(key) = key_path {
        key_str = key.to_string_lossy().to_string();
        args.extend_from_slice(&["-i", &key_str]);
    }
    local_str = local_path.to_string_lossy().to_string();
    args.push(&local_str);
    args.push(&remote);

    let output = if let Some(pwd) = password {
        let mut sshpass_args: Vec<&str> = vec!["-p", pwd, "scp"];
        sshpass_args.extend_from_slice(&args);
        Command::new("sshpass")
            .args(&sshpass_args)
            .output()
            .map_err(|e| format!("sshpass 执行失败: {}", e))?
    } else {
        Command::new("scp")
            .args(&args)
            .output()
            .map_err(|e| format!("scp 执行失败: {}", e))?
    };

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "上传失败: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// 部署 VLESS + Reality
fn deploy_vless_reality(
    params: &DeployParams,
    logs: &mut Vec<String>,
) -> Result<DeployResult, String> {
    let port = params.port_out.unwrap_or(443);
    let uuid = generate_uuid();
    let short_id = generate_password(16);
    let server_name = params.domain.as_deref().unwrap_or("www.microsoft.com");

    logs.push(format!("部署 VLESS + Reality 到 {}:{}", params.host, port));
    logs.push(format!("UUID: {}", uuid));
    logs.push(format!("Short ID: {}", short_id));
    logs.push(format!("Server Name: {}", server_name));

    // 1. 更新系统
    logs.push("更新系统包...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "apt-get update && apt-get upgrade -y",
    )?;

    // 2. 安装 Xray
    logs.push("安装 Xray-core...".to_string());
    ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
        "bash -c \"curl -fsSL https://github.com/XTLS/Xray-install/raw/main/install-release.sh | bash\"")?;

    // 3. 生成配置
    let config = serde_json::json!({
        "log": {
            "loglevel": "warning"
        },
        "inbounds": [{
            "port": port,
            "protocol": "vless",
            "settings": {
                "clients": [{
                    "id": uuid,
                    "flow": "xtls-rprx-vision"
                }],
                "decryption": "none"
            },
            "streamSettings": {
                "network": "tcp",
                "security": "reality",
                "realitySettings": {
                    "show": false,
                    "dest": format!("{}:443", server_name),
                    "xver": 0,
                    "serverNames": [server_name],
                    "privateKey": "",
                    "shortIds": [short_id]
                }
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"]
            }
        }],
        "outbounds": [{
            "protocol": "freedom",
            "tag": "direct"
        }, {
            "protocol": "blackhole",
            "tag": "block"
        }],
        "routing": {
            "rules": [{
                "type": "field",
                "outboundTag": "block",
                "protocol": ["bittorrent"]
            }]
        }
    });

    // 4. 上传配置
    let config_str =
        serde_json::to_string_pretty(&config).map_err(|e| format!("配置序列化失败: {}", e))?;
    let config_path = std::env::temp_dir().join("xray_config.json");
    std::fs::write(&config_path, &config_str).map_err(|e| format!("写入临时配置失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &config_path,
        "/usr/local/etc/xray/config.json",
    )?;

    // 5. 重启服务
    logs.push("重启 Xray 服务...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "systemctl restart xray && systemctl enable xray",
    )?;

    // 6. 配置防火墙
    if params.configure_firewall {
        logs.push("配置防火墙...".to_string());
        let _ = ssh_exec(
            &params.host,
            params.port,
            &params.user,
            params.password.as_deref(),
            params.key_path.as_ref(),
            &format!("ufw allow {}/tcp", port),
        );
    }

    // 7. 安装 BBR
    if params.install_bbr {
        logs.push("安装 BBR 加速...".to_string());
        let _ = ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
            "bash -c \"echo 'net.core.default_qdisc=fq' >> /etc/sysctl.conf && echo 'net.ipv4.tcp_congestion_control=bbr' >> /etc/sysctl.conf && sysctl -p\"");
    }

    // 生成连接 URI
    let uri = format!("vless://{}@{}:{}?encryption=none&flow=xtls-rprx-vision&security=reality&sni={}&fp=chrome&pbk=&sid={}&type=tcp#GHBoost-Reality", 
        uuid, params.host, port, server_name, short_id);

    // 清理临时文件
    let _ = std::fs::remove_file(&config_path);

    let out_logs = logs.to_vec();
    Ok(DeployResult {
        success: true,
        protocol: "vless-reality".to_string(),
        server: params.host.clone(),
        port,
        uri,
        config_json: config,
        logs: out_logs,
        error: None,
    })
}

/// 部署 VLESS + WebSocket
fn deploy_vless_ws(params: &DeployParams, logs: &mut Vec<String>) -> Result<DeployResult, String> {
    let port = params.port_out.unwrap_or(443);
    let uuid = generate_uuid();
    let path = format!("/{}", generate_password(8));

    logs.push(format!(
        "部署 VLESS + WebSocket 到 {}:{}",
        params.host, port
    ));
    logs.push(format!("UUID: {}", uuid));
    logs.push(format!("Path: {}", path));

    // 1. 更新系统
    logs.push("更新系统包...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "apt-get update && apt-get upgrade -y",
    )?;

    // 2. 安装 Xray
    logs.push("安装 Xray-core...".to_string());
    ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
        "bash -c \"curl -fsSL https://github.com/XTLS/Xray-install/raw/main/install-release.sh | bash\"")?;

    // 3. 生成配置
    let config = serde_json::json!({
        "log": {
            "loglevel": "warning"
        },
        "inbounds": [{
            "port": port,
            "protocol": "vless",
            "settings": {
                "clients": [{
                    "id": uuid
                }],
                "decryption": "none"
            },
            "streamSettings": {
                "network": "ws",
                "wsSettings": {
                    "path": path,
                    "headers": {
                        "Host": params.domain.as_deref().unwrap_or("")
                    }
                }
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"]
            }
        }],
        "outbounds": [{
            "protocol": "freedom",
            "tag": "direct"
        }, {
            "protocol": "blackhole",
            "tag": "block"
        }]
    });

    // 4. 上传配置
    let config_str =
        serde_json::to_string_pretty(&config).map_err(|e| format!("配置序列化失败: {}", e))?;
    let config_path = std::env::temp_dir().join("xray_config.json");
    std::fs::write(&config_path, &config_str).map_err(|e| format!("写入临时配置失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &config_path,
        "/usr/local/etc/xray/config.json",
    )?;

    // 5. 重启服务
    logs.push("重启 Xray 服务...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "systemctl restart xray && systemctl enable xray",
    )?;

    // 6. 配置防火墙
    if params.configure_firewall {
        logs.push("配置防火墙...".to_string());
        let _ = ssh_exec(
            &params.host,
            params.port,
            &params.user,
            params.password.as_deref(),
            params.key_path.as_ref(),
            &format!("ufw allow {}/tcp", port),
        );
    }

    // 生成连接 URI
    let domain = params.domain.as_deref().unwrap_or(&params.host);
    let uri = format!(
        "vless://{}@{}:{}?encryption=none&security=none&type=ws&path={}#GHBoost-WS",
        uuid, params.host, port, path
    );

    // 清理临时文件
    let _ = std::fs::remove_file(&config_path);

    let out_logs = logs.to_vec();
    Ok(DeployResult {
        success: true,
        protocol: "vless-ws".to_string(),
        server: params.host.clone(),
        port,
        uri,
        config_json: config,
        logs: out_logs,
        error: None,
    })
}

/// 部署 Trojan
fn deploy_trojan(params: &DeployParams, logs: &mut Vec<String>) -> Result<DeployResult, String> {
    let port = params.port_out.unwrap_or(443);
    let password = generate_password(16);

    logs.push(format!("部署 Trojan 到 {}:{}", params.host, port));
    logs.push(format!("Password: {}", password));

    // 1. 更新系统
    logs.push("更新系统包...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "apt-get update && apt-get upgrade -y",
    )?;

    // 2. 安装 Xray
    logs.push("安装 Xray-core...".to_string());
    ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
        "bash -c \"curl -fsSL https://github.com/XTLS/Xray-install/raw/main/install-release.sh | bash\"")?;

    // 3. 生成配置
    let config = serde_json::json!({
        "log": {
            "loglevel": "warning"
        },
        "inbounds": [{
            "port": port,
            "protocol": "trojan",
            "settings": {
                "clients": [{
                    "password": password
                }]
            },
            "streamSettings": {
                "network": "tcp",
                "security": "none"
            },
            "sniffing": {
                "enabled": true,
                "destOverride": ["http", "tls", "quic"]
            }
        }],
        "outbounds": [{
            "protocol": "freedom",
            "tag": "direct"
        }, {
            "protocol": "blackhole",
            "tag": "block"
        }]
    });

    // 4. 上传配置
    let config_str =
        serde_json::to_string_pretty(&config).map_err(|e| format!("配置序列化失败: {}", e))?;
    let config_path = std::env::temp_dir().join("xray_config.json");
    std::fs::write(&config_path, &config_str).map_err(|e| format!("写入临时配置失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &config_path,
        "/usr/local/etc/xray/config.json",
    )?;

    // 5. 重启服务
    logs.push("重启 Xray 服务...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "systemctl restart xray && systemctl enable xray",
    )?;

    // 6. 配置防火墙
    if params.configure_firewall {
        logs.push("配置防火墙...".to_string());
        let _ = ssh_exec(
            &params.host,
            params.port,
            &params.user,
            params.password.as_deref(),
            params.key_path.as_ref(),
            &format!("ufw allow {}/tcp", port),
        );
    }

    // 生成连接 URI
    let uri = format!(
        "trojan://{}@{}:{}?security=none&type=tcp#GHBoost-Trojan",
        password, params.host, port
    );

    // 清理临时文件
    let _ = std::fs::remove_file(&config_path);

    let out_logs = logs.to_vec();
    Ok(DeployResult {
        success: true,
        protocol: "trojan".to_string(),
        server: params.host.clone(),
        port,
        uri,
        config_json: config,
        logs: out_logs,
        error: None,
    })
}

/// 部署 Shadowsocks
fn deploy_shadowsocks(
    params: &DeployParams,
    logs: &mut Vec<String>,
) -> Result<DeployResult, String> {
    let port = params.port_out.unwrap_or(8388);
    let password = generate_password(16);
    let method = "aes-256-gcm";

    logs.push(format!("部署 Shadowsocks 到 {}:{}", params.host, port));
    logs.push(format!("Password: {}", password));
    logs.push(format!("Method: {}", method));

    // 1. 更新系统
    logs.push("更新系统包...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "apt-get update && apt-get upgrade -y",
    )?;

    // 2. 安装 Shadowsocks-rust
    logs.push("安装 Shadowsocks-rust...".to_string());
    ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
        "bash -c \"curl -fsSL https://github.com/shadowsocks/shadowsocks-rust/releases/latest/download/shadowsocks-v1.21.2.x86_64-unknown-linux-gnu.tar.gz | tar xz -C /usr/local/bin\"")?;

    // 3. 生成配置
    let config = serde_json::json!({
        "server": "0.0.0.0",
        "server_port": port,
        "password": password,
        "method": method,
        "timeout": 300,
        "fast_open": false,
        "mode": "tcp_and_udp",
        "no_delay": true
    });

    // 4. 上传配置
    let config_str =
        serde_json::to_string_pretty(&config).map_err(|e| format!("配置序列化失败: {}", e))?;
    let config_path = std::env::temp_dir().join("ss_config.json");
    std::fs::write(&config_path, &config_str).map_err(|e| format!("写入临时配置失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &config_path,
        "/etc/shadowsocks-rust/config.json",
    )?;

    // 5. 创建 systemd 服务
    let service = "[Unit]\nDescription=Shadowsocks-rust Server\nAfter=network.target\n\n[Service]\nType=simple\nExecStart=/usr/local/bin/ssserver -c /etc/shadowsocks-rust/config.json\nRestart=on-failure\nRestartSec=5s\n\n[Install]\nWantedBy=multi-user.target\n";
    let service_path = std::env::temp_dir().join("shadowsocks.service");
    std::fs::write(&service_path, service).map_err(|e| format!("写入服务文件失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &service_path,
        "/etc/systemd/system/shadowsocks.service",
    )?;

    // 6. 启动服务
    logs.push("启动 Shadowsocks 服务...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "systemctl daemon-reload && systemctl restart shadowsocks && systemctl enable shadowsocks",
    )?;

    // 7. 配置防火墙
    if params.configure_firewall {
        logs.push("配置防火墙...".to_string());
        let _ = ssh_exec(
            &params.host,
            params.port,
            &params.user,
            params.password.as_deref(),
            params.key_path.as_ref(),
            &format!("ufw allow {}/tcp && ufw allow {} udp", port, port),
        );
    }

    // 生成连接 URI
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", method, password).as_bytes(),
    );
    let uri = format!("ss://{}@{}:{}#GHBoost-SS", encoded, params.host, port);

    // 清理临时文件
    let _ = std::fs::remove_file(&config_path);
    let _ = std::fs::remove_file(&service_path);

    let out_logs = logs.to_vec();
    Ok(DeployResult {
        success: true,
        protocol: "shadowsocks".to_string(),
        server: params.host.clone(),
        port,
        uri,
        config_json: config,
        logs: out_logs,
        error: None,
    })
}

/// 部署 Hysteria2
fn deploy_hysteria2(params: &DeployParams, logs: &mut Vec<String>) -> Result<DeployResult, String> {
    let port = params.port_out.unwrap_or(8443);
    let password = generate_password(16);

    logs.push(format!("部署 Hysteria2 到 {}:{}", params.host, port));
    logs.push(format!("Password: {}", password));

    // 1. 更新系统
    logs.push("更新系统包...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "apt-get update && apt-get upgrade -y",
    )?;

    // 2. 安装 Hysteria2
    logs.push("安装 Hysteria2...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "bash -c \"curl -fsSL https://get.hy2.sh/ | bash\"",
    )?;

    // 3. 生成配置
    let listen = format!(":{}", port);
    let config = serde_json::json!({
        "listen": listen,
        "auth": {
            "password": password
        },
        "quic": {
            "initStreamReceiveWindow": 8388608,
            "maxStreamReceiveWindow": 8388608,
            "initConnReceiveWindow": 20971520,
            "maxConnReceiveWindow": 20971520,
            "maxIdleTimeout": "10s",
            "maxIncomingStreams": 1024,
            "disablePathMTUDiscovery": false
        },
        "bandwidth": {
            "up": "100 mbps",
            "down": "100 mbps"
        },
        "masquerade": {
            "type": "proxy",
            "proxy": {
                "url": "https://bing.com",
                "rewriteHost": true
            }
        }
    });

    // 4. 上传配置
    let config_str =
        serde_json::to_string_pretty(&config).map_err(|e| format!("配置序列化失败: {}", e))?;
    let config_path = std::env::temp_dir().join("hysteria2_config.yaml");
    std::fs::write(&config_path, &config_str).map_err(|e| format!("写入临时配置失败: {}", e))?;

    scp_upload(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        &config_path,
        "/etc/hysteria2/config.yaml",
    )?;

    // 5. 启动服务
    logs.push("启动 Hysteria2 服务...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "systemctl restart hysteria2 && systemctl enable hysteria2",
    )?;

    // 6. 配置防火墙
    if params.configure_firewall {
        logs.push("配置防火墙...".to_string());
        let _ = ssh_exec(
            &params.host,
            params.port,
            &params.user,
            params.password.as_deref(),
            params.key_path.as_ref(),
            &format!("ufw allow {}udp", port),
        );
    }

    // 安装 BBR
    if params.install_bbr {
        logs.push("安装 BBR 加速...".to_string());
        let _ = ssh_exec(&params.host, params.port, &params.user, params.password.as_deref(), params.key_path.as_ref(),
            "bash -c \"echo 'net.core.default_qdisc=fq' >> /etc/sysctl.conf && echo 'net.ipv4.tcp_congestion_control=bbr' >> /etc/sysctl.conf && sysctl -p\"");
    }

    // 生成连接 URI
    let uri = format!(
        "hysteria2://{}@{}:{}?insecure=1#GHBoost-Hy2",
        password, params.host, port
    );

    // 清理临时文件
    let _ = std::fs::remove_file(&config_path);

    let out_logs = logs.to_vec();
    Ok(DeployResult {
        success: true,
        protocol: "hysteria2".to_string(),
        server: params.host.clone(),
        port,
        uri,
        config_json: config,
        logs: out_logs,
        error: None,
    })
}

/// 一键部署入口
pub async fn deploy_core(params: DeployParams) -> Result<DeployResult, String> {
    let mut logs: Vec<String> = Vec::new();

    logs.push(format!(
        "开始部署 {} 到 {}:{}",
        params.protocol, params.host, params.port
    ));

    // 测试 SSH 连接
    logs.push("测试 SSH 连接...".to_string());
    ssh_exec(
        &params.host,
        params.port,
        &params.user,
        params.password.as_deref(),
        params.key_path.as_ref(),
        "echo ok",
    )
    .map_err(|e| format!("SSH 连接失败: {}", e))?;

    // 根据协议分发
    match params.protocol {
        Protocol::VlessReality => deploy_vless_reality(&params, &mut logs),
        Protocol::VlessWs => deploy_vless_ws(&params, &mut logs),
        Protocol::Trojan => deploy_trojan(&params, &mut logs),
        Protocol::Shadowsocks => deploy_shadowsocks(&params, &mut logs),
        Protocol::Hysteria2 => deploy_hysteria2(&params, &mut logs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_uuid() {
        let uuid = generate_uuid();
        assert_eq!(uuid.len(), 36);
        assert!(uuid.contains('-'));
    }

    #[test]
    fn test_generate_password() {
        let pwd = generate_password(16);
        assert_eq!(pwd.len(), 16);
        assert!(pwd.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_protocol_display() {
        assert_eq!(Protocol::VlessReality.to_string(), "vless-reality");
        assert_eq!(Protocol::Hysteria2.to_string(), "hysteria2");
    }
}
