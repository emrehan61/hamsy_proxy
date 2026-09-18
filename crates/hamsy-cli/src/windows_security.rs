//! Windows-only protections for the private HAR viewer state directory.
//!
//! The viewer writes a bearer token and a discovery record into this directory.
//! Windows file modes do not provide an equivalent of Unix `0700`/`0600`, so
//! these helpers create an explicit protected DACL and verify it every time the
//! directory or a file is opened.  The checks deliberately reject reparse
//! points and multiply-linked files to avoid path redirection and aliasing.

#![cfg(windows)]

use anyhow::{anyhow, bail, Context, Result};
use std::{
    ffi::{c_void, OsStr},
    fs::{self, File},
    io::Seek,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, GENERIC_READ, GENERIC_WRITE,
        HANDLE, INVALID_HANDLE_VALUE,
    },
    Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW,
        GetSecurityInfo, SE_FILE_OBJECT,
    },
    Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, GetTokenInformation, TokenUser,
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL_SIZE_INFORMATION, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, TOKEN_QUERY,
        TOKEN_USER,
    },
    Storage::FileSystem::{
        CreateDirectoryW, CreateFileW, GetFileInformationByHandle, MoveFileExW,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        OPEN_ALWAYS, OPEN_EXISTING,
    },
    System::{
        SystemServices::ACCESS_ALLOWED_ACE_TYPE,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

const SYSTEM_SID: &str = "S-1-5-18";
const FILE_ALL_ACCESS: u32 = 0x001F01FF;
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
const READ_CONTROL: u32 = 0x0002_0000;

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn wide_str(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn win_error(operation: &str) -> anyhow::Error {
    anyhow!(
        "{operation}: {}",
        std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
    )
}

struct LocalSid(*mut c_void);
impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn current_user_sid() -> Result<Vec<u8>> {
    let mut token: HANDLE = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(win_error("OpenProcessToken"));
    }
    let result = (|| {
        let mut length = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length);
        }
        if length == 0 {
            return Err(win_error("GetTokenInformation size"));
        }
        let mut bytes = vec![0u8; length as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                bytes.as_mut_ptr() as *mut c_void,
                length,
                &mut length,
            )
        } == 0
        {
            return Err(win_error("GetTokenInformation"));
        }
        let user = unsafe { &*(bytes.as_ptr() as *const TOKEN_USER) };
        let sid = unsafe {
            std::slice::from_raw_parts(user.User.Sid as *const u8, sid_length(user.User.Sid)?)
        };
        Ok(sid.to_vec())
    })();
    unsafe { CloseHandle(token) };
    result
}

fn sid_length(sid: *mut c_void) -> Result<usize> {
    if sid.is_null() {
        bail!("Windows token did not contain a user SID");
    }
    // The SID layout stores the sub-authority count in byte 1.  This avoids
    // relying on another optional advapi32 helper just to copy the SID.
    let count = unsafe { *((sid as *const u8).add(1)) } as usize;
    let length = 8usize
        .checked_add(count.checked_mul(4).context("invalid user SID")?)
        .context("invalid user SID length")?;
    Ok(length)
}

fn sid_string(sid: &[u8]) -> Result<String> {
    let mut text = null_mut();
    if unsafe {
        windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
            sid.as_ptr() as *mut c_void,
            &mut text,
        )
    } == 0
    {
        return Err(win_error("ConvertSidToStringSidW"));
    }
    let value = unsafe {
        let mut len = 0usize;
        while *text.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
    };
    unsafe { LocalFree(text as *mut c_void) };
    Ok(value)
}

fn descriptor_sddl() -> Result<Vec<u16>> {
    let sid = sid_string(&current_user_sid()?)?;
    // Protect the DACL from inheritance.  SYSTEM is retained for normal OS
    // administration; the current user is the only non-system principal.
    Ok(wide_str(&format!(
        "O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;{SYSTEM_SID})"
    )))
}

struct SecurityDescriptor(*mut c_void);
impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn security_attributes(sddl: &[u16]) -> Result<(SecurityDescriptor, SECURITY_ATTRIBUTES)> {
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(win_error(
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        ));
    }
    let owned = SecurityDescriptor(descriptor);
    let attrs = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: owned.0,
        bInheritHandle: 0,
    };
    Ok((owned, attrs))
}

fn validate_acl(path: &Path, file: &File) -> Result<()> {
    let user_sid = current_user_sid()?;
    let user_sid_ptr = user_sid.as_ptr() as *mut c_void;
    let system_sddl = wide_str(SYSTEM_SID);
    let mut system_sid = null_mut();
    if unsafe { ConvertStringSidToSidW(system_sddl.as_ptr(), &mut system_sid) } == 0 {
        return Err(win_error("ConvertStringSidToSidW"));
    }
    let system_sid = LocalSid(system_sid);

    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32).into());
    }
    let descriptor = LocalSid(descriptor);
    let mut owner_defaulted = 0;
    if unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut owner_defaulted) } == 0
        || owner.is_null()
        || unsafe { EqualSid(owner, user_sid_ptr) } == 0
    {
        bail!(
            "{} is not owned by the current Windows user",
            path.display()
        );
    }
    let mut present = 0;
    let mut defaulted = 0;
    if unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
        || present == 0
        || dacl.is_null()
    {
        bail!("{} does not have a private DACL", path.display());
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        bail!("{} has an inheritable DACL", path.display());
    }
    let mut info = ACL_SIZE_INFORMATION {
        AceCount: 0,
        AclBytesInUse: 0,
        AclBytesFree: 0,
    };
    if unsafe {
        GetAclInformation(
            dacl,
            &mut info as *mut _ as *mut c_void,
            size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        )
    } == 0
    {
        return Err(win_error("GetAclInformation"));
    }
    // The explicit DACL created above has exactly these two allow entries.
    // Files created below the directory receive the same protected DACL.
    if info.AceCount != 2 {
        bail!("{} has unexpected principals in its DACL", path.display());
    }
    let mut found_user = false;
    let mut found_system = false;
    for index in 0..info.AceCount {
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(win_error("GetAce"));
        }
        let header = unsafe { &*(ace as *const ACE_HEADER) };
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
            bail!("{} has a non-allow ACE", path.display());
        }
        let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
        if allowed.Mask != FILE_ALL_ACCESS {
            bail!("{} has a partial-access DACL entry", path.display());
        }
        let sid = &allowed.SidStart as *const u32 as *mut c_void;
        if unsafe { EqualSid(sid, user_sid_ptr) } != 0 {
            found_user = true;
        } else if unsafe { EqualSid(sid, system_sid.0) } != 0 {
            found_system = true;
        } else {
            bail!("{} has an untrusted DACL principal", path.display());
        }
    }
    if !found_user || !found_system {
        bail!("{} is missing its private DACL entries", path.display());
    }
    Ok(())
}

fn validate_handle(path: &Path, file: &File, directory: bool) -> Result<()> {
    let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
        return Err(win_error("GetFileInformationByHandle"));
    }
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("{} is a Windows reparse point", path.display());
    }
    let actual_directory = info.dwFileAttributes
        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY
        != 0;
    if actual_directory != directory {
        bail!("{} has an unexpected file type", path.display());
    }
    if !directory && info.nNumberOfLinks != 1 {
        bail!("{} has multiple hard links", path.display());
    }
    validate_acl(path, file)?;
    Ok(())
}

fn open_directory(path: &Path) -> Result<File> {
    let path_w = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            FILE_READ_ATTRIBUTES | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(win_error("CreateFileW directory"));
    }
    Ok(unsafe { File::from_raw_handle(handle as _) })
}

pub fn private_dir(data_dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(data_dir)?;
    let data_dir_file = open_directory(data_dir)
        .with_context(|| format!("cannot open data directory {}", data_dir.display()))?;
    // The user-selected parent may be permissive, but it must not redirect
    // the private state path through a junction or another reparse point.
    let mut parent_info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe {
        GetFileInformationByHandle(data_dir_file.as_raw_handle() as HANDLE, &mut parent_info)
    } == 0
        || parent_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!("{} is a Windows reparse point", data_dir.display());
    }
    let dir = data_dir.join("har-viewer");
    let sddl = descriptor_sddl()?;
    let (descriptor, mut attrs) = security_attributes(&sddl)?;
    let dir_w = wide(dir.as_os_str());
    let created = unsafe { CreateDirectoryW(dir_w.as_ptr(), &mut attrs) };
    if created == 0 && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return Err(win_error("CreateDirectoryW"));
    }
    drop(descriptor);
    let handle = open_directory(&dir)
        .with_context(|| format!("cannot open private directory {}", dir.display()))?;
    validate_handle(&dir, &handle, true)?;
    Ok(dir)
}

pub fn validate_private_dir(dir: &Path) -> Result<()> {
    let handle = open_directory(dir)
        .with_context(|| format!("cannot open private directory {}", dir.display()))?;
    validate_handle(dir, &handle, true)
}

pub fn open_private_file(path: &Path, append: bool, create: bool) -> Result<File> {
    let sddl = descriptor_sddl()?;
    let (descriptor, mut attrs) = security_attributes(&sddl)?;
    let path_w = wide(path.as_os_str());
    // The log is opened at EOF below, but it is also truncated when it grows
    // past the bound, so retain ordinary write rights on the handle.
    let access = GENERIC_READ | GENERIC_WRITE;
    let disposition = if create { OPEN_ALWAYS } else { OPEN_EXISTING };
    let handle = unsafe {
        CreateFileW(
            path_w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &mut attrs,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    drop(descriptor);
    if handle == INVALID_HANDLE_VALUE {
        return Err(win_error("CreateFileW"));
    }
    let mut file = unsafe { File::from_raw_handle(handle as _) };
    validate_handle(path, &file, false)?;
    if append {
        file.seek(std::io::SeekFrom::End(0))?;
    }
    Ok(file)
}

pub fn replace_file(from: &Path, to: &Path) -> Result<()> {
    let from_w = wide(from.as_os_str());
    let to_w = wide(to.as_os_str());
    if unsafe {
        MoveFileExW(
            from_w.as_ptr(),
            to_w.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(win_error("MoveFileExW"));
    }
    // A replacement must retain the same private guarantees as the original
    // discovery record, even if another process raced the destination path.
    let file = open_private_file(to, false, false)?;
    drop(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn private_state_rejects_aliases_and_keeps_private_acl() {
        let temp = tempfile::tempdir().unwrap();
        let dir = private_dir(temp.path()).unwrap();
        let path = dir.join("service.lock");
        let mut file = open_private_file(&path, false, true).unwrap();
        file.write_all(b"private").unwrap();
        drop(file);
        validate_private_dir(&dir).unwrap();

        let hard_link = dir.join("service-alias.lock");
        if fs::hard_link(&path, &hard_link).is_ok() {
            assert!(open_private_file(&path, false, false).is_err());
        }

        // Symlink creation can be disabled by Windows developer-mode policy;
        // when available, opening the link must still fail closed.
        use std::os::windows::fs::symlink_file;
        let symlink = dir.join("service-link.lock");
        if symlink_file(&path, &symlink).is_ok() {
            assert!(open_private_file(&symlink, false, false).is_err());
        }
    }

    #[test]
    fn discovery_replacement_is_atomic_and_keeps_acl() {
        let temp = tempfile::tempdir().unwrap();
        let dir = private_dir(temp.path()).unwrap();
        let destination = dir.join("service.json");
        let source = dir.join("service-next.tmp");
        let mut old = open_private_file(&destination, false, true).unwrap();
        old.write_all(b"old").unwrap();
        let mut next = open_private_file(&source, false, true).unwrap();
        next.write_all(b"new").unwrap();
        next.sync_all().unwrap();
        drop(next);
        replace_file(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert!(!source.exists());
        validate_private_dir(&dir).unwrap();
    }

    #[test]
    fn rejects_permissive_acl_when_icacls_is_available() {
        let temp = tempfile::tempdir().unwrap();
        let dir = private_dir(temp.path()).unwrap();
        let status = std::process::Command::new("icacls")
            .arg(&dir)
            .args(["/grant", "*S-1-1-0:(F)"])
            .status();
        if status.map(|status| status.success()).unwrap_or(false) {
            assert!(validate_private_dir(&dir).is_err());
        }
    }

    #[test]
    fn append_handle_can_rotate_log_without_stale_offset() {
        let temp = tempfile::tempdir().unwrap();
        let dir = private_dir(temp.path()).unwrap();
        let path = dir.join("viewer.log");
        let mut file = open_private_file(&path, true, true).unwrap();
        file.write_all(b"old log").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let mut file = open_private_file(&path, true, false).unwrap();
        file.set_len(0).unwrap();
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        file.write_all(b"new log").unwrap();
        drop(file);
        assert_eq!(fs::read(path).unwrap(), b"new log");
    }
}
