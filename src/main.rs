#[macro_use]
extern crate serde_json;
#[macro_use]
extern crate lazy_static;

use actix_files::NamedFile;
use actix_web::{web, web::Json, App, HttpRequest, HttpServer, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::sync::RwLock;
use std::time::{Duration, SystemTime};
use std::{thread, time};
use structopt::StructOpt;
use systemstat::data::NetworkStats;
use systemstat::{Platform, System};

#[derive(StructOpt, Debug)]
#[structopt(name = "pi-panel", about = "SBC(single board computer) panel")]
struct CommandArgs {
    #[structopt(
        short = "-b",
        long = "--bind",
        help = "interface to which the server will bind"
    )]
    interface: String,
    #[structopt(
        short = "-m",
        long = "--mountBase",
        default_value = "/mnt/",
        help = "parent directory of mount point when using relative path"
    )]
    mount_base: String,
}

lazy_static! {
    static ref MOUNT_BASE_PATH: RwLock<String> = RwLock::new(String::new());
}

fn init_mount_base_path(mut mount_base_path: String) {
    if !mount_base_path.ends_with("/") {
        mount_base_path.push('/');
    }
    (*MOUNT_BASE_PATH)
        .write()
        .expect("Failed to initialize MOUNT_BASE_PATH")
        .push_str(&mount_base_path);
}

fn main() {
    let args = CommandArgs::from_args();
    init_mount_base_path(args.mount_base.to_owned());

    HttpServer::new(|| {
        App::new()
            .route("/shutdown", web::get().to(shutdown))
            .route("/reboot", web::get().to(reboot))
            .route("/status_data", web::get().to(status_data))
            .route("/disk_info", web::get().to(disk_info))
            .route("/mount_disk", web::post().to(mount_disk))
            .route("/remove_disk", web::post().to(remove_disk))
            .route("", web::get().to(ui))
            .route(r"/{tail:.*}", web::get().to(ui))
    })
    .bind(&args.interface)
    .expect(&format!("Can not bind to interface[{}]", args.interface))
    .run()
    .expect("Failed to boot server");
}

fn ui(req: HttpRequest) -> actix_web::Result<NamedFile> {
    let mut file_name = req.match_info().get("tail").unwrap_or("index.html");
    if file_name.is_empty() {
        file_name = "index.html";
    }
    Ok(NamedFile::open("ui/".to_owned() + file_name)?)
}

fn status_data() -> Result<Json<serde_json::map::Map<String, serde_json::Value>>> {
    let sys = System::new();
    let temperature = sys.cpu_temp().unwrap_or(0.0);
    let mut status_data = serde_json::map::Map::new();
    status_data.insert(
        "temperature".to_string(),
        serde_json::to_value(temperature).unwrap(),
    );
    if let Ok(mem) = sys.memory() {
        status_data.insert(
            "memory".to_string(),
            json!({"total": mem.total.as_u64(), "free": mem.free.as_u64()}),
        );
    }
    let mut old_net_stat: Option<NetworkStats> = Option::None;
    let mut internet_interface = String::from("eth0");
    let start_time = SystemTime::now();
    if let Ok(net_interface_map) = sys.networks() {
        for net_interface in net_interface_map.keys() {
            if net_interface != "lo" {
                if let Ok(data) = sys.network_stats(&net_interface) {
                    old_net_stat = Option::Some(data);
                    internet_interface = net_interface.to_string();
                }
            }
        }
    }
    let mut cpu_utilization = Vec::new();
    let cpu = sys.cpu_load();
    thread::sleep(time::Duration::from_millis(1000));
    if let Ok(cpu) = cpu {
        if let Ok(cpu) = cpu.done() {
            for core in cpu {
                let utilization = (core.user + core.system + core.interrupt) * 100.0;
                cpu_utilization.push(utilization);
            }
        }
    }
    status_data.insert(
        "cores".to_string(),
        serde_json::to_value(cpu_utilization).unwrap_or(json!(null)),
    );
    status_data.insert("net_traffic".to_string(), json!(null));
    if let Some(old_net_stat) = old_net_stat {
        if let Ok(new_net_stat) = sys.network_stats(&internet_interface) {
            let millis = start_time
                .elapsed()
                .unwrap_or(Duration::from_millis(1500))
                .as_millis();
            let rx_bytes = new_net_stat.rx_bytes.as_u64() - old_net_stat.rx_bytes.as_u64();
            let tx_bytes = new_net_stat.tx_bytes.as_u64() - old_net_stat.tx_bytes.as_u64();
            status_data.insert(
                "net_traffic".to_string(),
                json!({"time": millis as u64, "traffic": [rx_bytes, tx_bytes]}),
            );
        }
    }
    Ok(Json(status_data))
}

#[derive(Serialize, Deserialize)]
struct DiskInfo {
    vendor: serde_json::Value,
    kname: serde_json::Value,
    device_name: serde_json::Value,
    label: serde_json::Value,
    fs_type: serde_json::Value,
    size: serde_json::Value,
    mount_point: serde_json::Value,
}

fn disk_info() -> Result<Json<serde_json::Value>> {
    let blk_info = Command::new("lsblk")
        .args(&[
            "-J",
            "-o",
            "VENDOR,KNAME,NAME,TYPE,RM,MOUNTPOINT,LABEL,MODEL,FSTYPE,SIZE,TRAN",
        ])
        .output();
    if let Ok(blk_info) = blk_info {
        if blk_info.status.success() {
            let blk_info_json: serde_json::Value = serde_json::from_slice(&blk_info.stdout)
                .expect("Failed to convert lsblk info to json");
            let devices = blk_info_json
                .get("blockdevices")
                .unwrap()
                .as_array()
                .unwrap();
            let mut disk_info_list = Vec::new();
            for device in devices {
                let tran = device.get("tran");
                if tran != Some(&json!("usb")) {
                    continue;
                }
                let children = device.get("children");
                if children.is_none() {
                    continue;
                }
                let vendor = device.get("vendor").unwrap_or(&json!(null));
                let model = device.get("model").unwrap_or(&json!(null));
                let children = children.unwrap().as_array().unwrap();
                for child in children {
                    let size = child.get("size").unwrap_or(&json!(null));
                    let mount_point = child.get("mountpoint").unwrap_or(&json!(null));
                    let label = child.get("label").unwrap_or(&json!(null));
                    let fs_type = child.get("fstype").unwrap_or(&json!(null));
                    let name = child.get("kname").unwrap_or(&json!(null));
                    let disk_info = DiskInfo {
                        kname: name.to_owned(),
                        size: size.to_owned(),
                        vendor: vendor.to_owned(),
                        device_name: model.to_owned(),
                        label: label.to_owned(),
                        mount_point: mount_point.to_owned(),
                        fs_type: fs_type.to_owned(),
                    };
                    disk_info_list.push(disk_info);
                }
            }
            Ok(Json(json!({"code": 0,"msg": "", "data": disk_info_list})))
        } else {
            Ok(Json(
                json!({"code": -1, "msg": String::from_utf8_lossy(&blk_info.stderr).to_owned()}),
            ))
        }
    } else {
        Ok(Json(json!({"code": -1, "msg": "命令执行失败"})))
    }
}

fn mount_disk(disk_info: Json<DiskInfo>) -> Result<Json<serde_json::Value>> {
    if let Some(mount_point) = disk_info.mount_point.as_str() {
        let mut mount_point = mount_point.trim().trim_start_matches('.').to_owned();
        let base_path = &*(MOUNT_BASE_PATH
            .read()
            .expect("Failed to read MOUNT_BASE_PATH"));
        if mount_point.starts_with("/") {
            if mount_point.len() == base_path.len() || !mount_point.starts_with(base_path) {
                // 错误路径
                return Ok(Json(json!({"code": -1, "msg": "错误的挂载点"})));
            }
        } else {
            mount_point.insert_str(0, &base_path);
        }
        let dest_path = std::path::Path::new(&mount_point);
        if !dest_path.exists() {
            // 挂载点路径不存在
            return Ok(Json(json!({"code": -1, "msg": "挂载点路径不存在"})));
        } else if !dest_path.is_dir() {
            // 不是目录
            return Ok(Json(json!({"code": -1, "msg": "挂载点路径不是一个目录！"})));
        }
        let fs_type = disk_info.fs_type.as_str().unwrap_or("");
        let mut args = vec![
            "/dev/".to_owned() + disk_info.kname.as_str().unwrap(),
            mount_point.clone(),
        ];
        if fs_type == "vfat" {
            args.push(String::from("-o"));
            args.push(String::from("rw,umask=0000"));
        }
        let mount_output = Command::new("mount").args(&args).output();
        if mount_output.is_ok() {
            let mount_output = mount_output.unwrap();
            if mount_output.status.success() {
                Ok(Json(
                    json!({"code": 0, "msg": "成功", "mount_point": dest_path}),
                ))
            } else {
                Ok(Json(
                    json!({"code": -1, "msg":format!("mount命令异常退出，错误信息:\n{}", String::from_utf8_lossy(&mount_output.stderr).to_string())}),
                ))
            }
        } else {
            Ok(Json(json!({"code": -1, "msg": "mount命令执行失败"})))
        }
    } else {
        Ok(Json(json!({"code": -1, "msg": "参数错误，未指定挂载点"})))
    }
}

fn remove_disk(disk_info: Json<DiskInfo>) -> Result<Json<serde_json::Value>> {
    if let Some(mount_point) = disk_info.mount_point.as_str() {
        let mut mount_point = mount_point.trim().trim_start_matches('.').to_owned();
        let base_path = &*(MOUNT_BASE_PATH
            .read()
            .expect("Failed to read MOUNT_BASE_PATH"));
        if mount_point.starts_with("/") {
            if mount_point.len() <= base_path.len() || !mount_point.starts_with(base_path) {
                // 错误路径
                return Ok(Json(json!({"code": -1, "msg": "错误的挂载点"})));
            }
        } else {
            mount_point.insert_str(0, base_path);
        }
        let dest_path = std::path::Path::new(&mount_point);
        if !dest_path.exists() {
            // 挂载点路径不存在
            return Ok(Json(json!({"code": -1, "msg": "挂载点路径不存在"})));
        } else if !dest_path.is_dir() {
            // 不是目录
            return Ok(Json(json!({"code": -1, "msg": "挂载点路径不是一个目录！"})));
        }
        let umount_output = Command::new("umount").arg(&mount_point).output();
        if umount_output.is_ok() {
            let umount_output = umount_output.unwrap();
            if umount_output.status.success() {
                Ok(Json(json!({"code": 0, "msg": "成功"})))
            } else {
                Ok(Json(
                    json!({"code": -1, "msg": format!("umount命令异常退出，错误信息:\n{}", String::from_utf8_lossy(&umount_output.stderr).to_string())}),
                ))
            }
        } else {
            Ok(Json(json!({"code": -1, "msg": "umount命令执行失败"})))
        }
    } else {
        Ok(Json(json!({"code": -1, "msg": "参数错误，未指定挂载点"})))
    }
}

fn shutdown() -> Result<Json<serde_json::Value>> {
    let shutdown_output = Command::new("shutdown").args(&["-h", "now"]).output();
    if shutdown_output.is_ok() {
        let shutdown_output = shutdown_output.unwrap();
        if shutdown_output.status.success() {
            Ok(Json(json!({"code": 0, "msg": "关机中..."})))
        } else {
            Ok(Json(
                json!({"code": 0, "msg": format!("shutdown命令异常退出，错误信息：\n{}", String::from_utf8_lossy(&shutdown_output.stderr).to_string())}),
            ))
        }
    } else {
        Ok(Json(json!({"code": -1, "msg": "shutdown命令执行失败"})))
    }
}

fn reboot() -> Result<Json<serde_json::Value>> {
    let shutdown_output = Command::new("reboot").output();
    if shutdown_output.is_ok() {
        let shutdown_output = shutdown_output.unwrap();
        if shutdown_output.status.success() {
            Ok(Json(json!({"code": 0, "msg": "重启中..."})))
        } else {
            Ok(Json(
                json!({"code": -1, "msg": format!("reboot命令异常退出，错误信息：\n{}", String::from_utf8_lossy(&shutdown_output.stderr).to_string())}),
            ))
        }
    } else {
        Ok(Json(json!({"code": -1, "msg": "reboot命令执行失败"})))
    }
}
