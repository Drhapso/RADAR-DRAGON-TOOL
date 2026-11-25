//! Lector directo de NTFS MFT y USN Journal para Windows.
//! Utiliza DeviceIoControl con FSCTL_ENUM_USN_DATA para indexar millones
//! de archivos en segundos, replicando la técnica central de Everything.

#[cfg(windows)]
pub mod win {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::time::Instant;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use dragon_core::{FileRecord, ScanMethod, ScanMetrics};

    // Estructuras nativas del kernel NTFS USN Journal
    pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900f4;
    pub const FSCTL_ENUM_USN_DATA: u32 = 0x000900b3;

    #[repr(C)]
    #[derive(Debug, Copy, Clone, Default)]
    pub struct USN_JOURNAL_DATA_V0 {
        pub usn_journal_id: u64,
        pub first_usn: i64,
        pub next_usn: i64,
        pub lowest_valid_usn: i64,
        pub max_usn: i64,
        pub maximum_size: u64,
        pub allocation_delta: u64,
    }

    #[repr(C)]
    #[derive(Debug, Copy, Clone, Default)]
    pub struct MFT_ENUM_DATA_V0 {
        pub start_file_reference_number: u64,
        pub low_usn: i64,
        pub high_usn: i64,
    }

    #[repr(C)]
    pub struct USN_RECORD_V2 {
        pub record_length: u32,
        pub major_version: u16,
        pub minor_version: u16,
        pub file_reference_number: u64,
        pub parent_file_reference_number: u64,
        pub usn: i64,
        pub time_stamp: i64,
        pub reason: u32,
        pub source_info: u32,
        pub security_id: u32,
        pub file_attributes: u32,
        pub file_name_length: u16,
        pub file_name_offset: u16,
    }

    /// Índice en memoria de archivos leídos del MFT.
    pub struct MftScanResult {
        pub root_volume: String,
        /// Mapa de ID de registro MFT a su registro
        pub records: HashMap<u64, (u64, String, u32)>, // (parent_id, name, attributes)
        pub metrics: ScanMetrics,
    }

    impl MftScanResult {
        /// Reconstruye la ruta absoluta ascendiendo por los IDs de padres.
        pub fn resolve_path(&self, id: u64) -> Option<PathBuf> {
            let mut parts = Vec::new();
            let mut curr_id = id;

            // Evitar ciclos infinitos con un límite de profundidad
            let mut depth = 0;
            while depth < 256 {
                if let Some((parent_id, name, _)) = self.records.get(&curr_id) {
                    parts.push(name.as_str());
                    // El directorio raíz de NTFS suele tener parent_id == id o 0x0005000000000005
                    if *parent_id == curr_id || *parent_id == 0 || (curr_id & 0x0000_FFFF_FFFF_FFFF) == 5 {
                        break;
                    }
                    curr_id = *parent_id;
                    depth += 1;
                } else {
                    break;
                }
            }

            if parts.is_empty() {
                return None;
            }

            parts.reverse();
            let mut full_path = PathBuf::from(&self.root_volume);
            for p in parts {
                if !p.is_empty() {
                    full_path.push(p);
                }
            }
            Some(full_path)
        }

        /// Convierte los registros indexados a `Vec<FileRecord>` listos para Tantivy o búsqueda.
        pub fn to_file_records(&self) -> Vec<FileRecord> {
            let mut result = Vec::with_capacity(self.records.len());
            for (&id, &(parent_id, ref name, attributes)) in &self.records {
                let is_dir = (attributes & FileRecord::ATTR_DIRECTORY) != 0;
                result.push(FileRecord {
                    id,
                    parent_id,
                    name: name.clone(),
                    path: None, // Se resuelve bajo demanda para ahorrar cientos de MB de RAM
                    size: 0,    // MFT básico omite tamaño exacto de data streams (se lee bajo demanda)
                    attributes,
                    modified_ms: 0,
                    is_directory: is_dir,
                });
            }
            result
        }
    }

    /// Escanea un volumen NTFS completo (por ejemplo "C:") leyendo el MFT.
    /// Requiere elevación / privilegios de Administrador.
    pub fn scan_volume_mft(volume_letter: char) -> Result<MftScanResult, String> {
        let start = Instant::now();
        let volume_str = format!(r"\\.\{}:", volume_letter);
        let root_dir = format!(r"{}:\", volume_letter);

        // 1. Abrir descriptor de acceso al volumen en crudo
        let wide_path: Vec<u16> = volume_str.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                HANDLE(std::ptr::null_mut()),
            )
        };

        let handle = match handle {
            Ok(h) if h != INVALID_HANDLE_VALUE => h,
            Ok(_) => return Err("Handle de volumen inválido".to_string()),
            Err(e) => {
                return Err(format!(
                    "Acceso denegado al volumen {} (Código: {}). Se requieren privilegios de Administrador para MFT.",
                    volume_str, e
                ));
            }
        };

        // Guardián para cerrar el handle al salir
        struct HandleGuard(HANDLE);
        impl Drop for HandleGuard {
            fn drop(&mut self) {
                unsafe { let _ = CloseHandle(self.0); }
            }
        }
        let _guard = HandleGuard(handle);

        // 2. Consultar el USN Journal activo
        let mut journal_data = USN_JOURNAL_DATA_V0::default();
        let mut bytes_returned = 0u32;
        let success = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some(&mut journal_data as *mut _ as *mut _),
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        if success.is_err() {
            return Err("No se pudo consultar el USN Journal en este volumen (¿No es NTFS o USN desactivado?)".to_string());
        }

        // 3. Enumerar todas las entradas del MFT secuencialmente
        let mut med = MFT_ENUM_DATA_V0 {
            start_file_reference_number: 0,
            low_usn: 0,
            high_usn: journal_data.next_usn,
        };

        // Búfer grande de 1 MB para lectura en ráfaga
        const BUF_SIZE: usize = 1024 * 1024;
        let mut buffer = vec![0u8; BUF_SIZE];
        let mut records: HashMap<u64, (u64, String, u32)> = HashMap::with_capacity(300_000);
        let mut file_count = 0;
        let mut dir_count = 0;

        loop {
            let mut out_bytes = 0u32;
            let res = unsafe {
                DeviceIoControl(
                    handle,
                    FSCTL_ENUM_USN_DATA,
                    Some(&med as *const _ as *const _),
                    std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                    Some(buffer.as_mut_ptr() as *mut _),
                    BUF_SIZE as u32,
                    Some(&mut out_bytes),
                    None,
                )
            };

            if res.is_err() || out_bytes < 8 {
                break;
            }

            // Los primeros 8 bytes devuelven el siguiente StartFileReferenceNumber
            let next_usn = unsafe { *(buffer.as_ptr() as *const u64) };
            let mut offset = 8usize;

            while offset + std::mem::size_of::<USN_RECORD_V2>() <= out_bytes as usize {
                let record = unsafe { &*(buffer.as_ptr().add(offset) as *const USN_RECORD_V2) };
                if record.record_length == 0 {
                    break;
                }

                if record.major_version == 2 {
                    let name_offset = offset + record.file_name_offset as usize;
                    let name_len_bytes = record.file_name_length as usize;

                    if name_offset + name_len_bytes <= out_bytes as usize {
                        let name_u16 = unsafe {
                            std::slice::from_raw_parts(
                                buffer.as_ptr().add(name_offset) as *const u16,
                                name_len_bytes / 2,
                            )
                        };
                        let name = OsString::from_wide(name_u16).to_string_lossy().into_owned();

                        // Omitir flujos alternativos NTFS (archivos con ':') y metarchivos raíz ($)
                        if !name.contains(':') {
                            let is_dir = (record.file_attributes & FileRecord::ATTR_DIRECTORY) != 0;
                            if is_dir {
                                dir_count += 1;
                            } else {
                                file_count += 1;
                            }
                            // Usamos los 48 bits bajos para el índice MFT real (los 16 altos son el número de secuencia)
                            let clean_id = record.file_reference_number & 0x0000_FFFF_FFFF_FFFF;
                            let clean_parent = record.parent_file_reference_number & 0x0000_FFFF_FFFF_FFFF;
                            records.insert(clean_id, (clean_parent, name, record.file_attributes));
                        }
                    }
                }

                offset += record.record_length as usize;
            }

            med.start_file_reference_number = next_usn;
        }

        let duration = start.elapsed();
        log::info!(
            "MFT scan de {} finalizado en {:?}: {} archivos, {} carpetas",
            volume_str, duration, file_count, dir_count
        );

        Ok(MftScanResult {
            root_volume: root_dir,
            records,
            metrics: ScanMetrics {
                volume_or_path: volume_str,
                total_files: file_count,
                total_directories: dir_count,
                duration_ms: duration.as_millis() as u64,
                scan_method: ScanMethod::NtfsMftDirect,
            },
        })
    }
}
