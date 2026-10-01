use super::*;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn test_compile_and_load_example_module() {
    // 1. Build example-module cdylib
    let cargo_status = Command::new("cargo")
        .args(["build", "--manifest-path", "../example-module/Cargo.toml"])
        .status()
        .expect("Failed to execute cargo build for example-module");
    assert!(cargo_status.success(), "Failed to build example-module");

    // Locate the built DLL / SO in target directory
    #[cfg(target_os = "windows")]
    let dll_name = "example_module.dll";
    #[cfg(target_os = "linux")]
    let dll_name = "libexample_module.so";
    #[cfg(target_os = "macos")]
    let dll_name = "libexample_module.dylib";

    // Find target directory
    let dll_path = PathBuf::from("../example-module/target/debug").join(dll_name);
    let fallback_path = PathBuf::from("target/debug").join(dll_name);
    let path = if dll_path.exists() {
        dll_path
    } else {
        fallback_path
    };

    assert!(path.exists(), "Compiled module not found at {:?}", path);

    // 2. Load module into ModuleManager
    let mut manager = ModuleManager::new();
    let meta = manager
        .load_module(&path)
        .expect("Failed to load example-module dll");

    assert_eq!(meta.name, "HORAS 24C16");
    assert_eq!(meta.apis, vec!["read_km", "save_km"]);
    assert_eq!(meta.ui.len(), 3);

    // 3. Prepare a simulated 24C16 binary file buffer (e.g. 2048 bytes)
    let mut data = vec![0u8; 2048];
    // Write original KM: say 100 km -> 100 * 0x3C = 6000 (0x00001770) at offset 2
    let original_km: u32 = 100;
    let original_raw: u32 = original_km * 0x3C;
    data[2..6].copy_from_slice(&original_raw.to_be_bytes());
    data[6] = 0xAA; // checkbit

    let mut binary_file = BinaryFile::new("ecu_dump.bin", data);

    // 4. Test read_km API
    let read_res = manager
        .execute("HORAS 24C16", "read_km", &mut binary_file, &serde_json::Value::Null)
        .expect("read_km execution failed");

    match read_res {
        ApiResponse::Ok(val) => {
            assert_eq!(val["km"], 100);
        }
        _ => panic!("Expected ApiResponse::Ok"),
    }

    // 5. Test save_km API to change KM to 250
    let save_res = manager
        .execute(
            "HORAS 24C16",
            "save_km",
            &mut binary_file,
            &serde_json::json!({ "km": 250 }),
        )
        .expect("save_km execution failed");

    match save_res {
        ApiResponse::Download { filename, data } => {
            assert_eq!(filename, "ecu_dump_km_250.bin");
            // Check that offset 2, 8, 0xE were written with 250 * 0x3C (15000 = 0x00003A98)
            let new_raw: u32 = 250 * 0x3C;
            let expected_bytes = new_raw.to_be_bytes();
            for loc in [0x02, 0x08, 0x0E] {
                assert_eq!(&data[loc..loc + 4], &expected_bytes);
                assert_eq!(data[loc + 4], 0xAA); // checkbit preserved
            }
        }
        _ => panic!("Expected ApiResponse::Download"),
    }
}