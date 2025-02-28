use crate::error;
use std::path::PathBuf;

//
// Non-Unix implementation
//

pub(crate) fn get_user_home_dir() -> Option<PathBuf> {
    homedir::get_my_home().unwrap_or_default()
}

pub(crate) fn is_root() -> bool {
    // TODO: implement some version of this for Windows
    false
}

pub(crate) fn get_effective_uid() -> Result<u32, error::Error> {
    error::unimp("get effective uid")
}

pub(crate) fn get_effective_gid() -> Result<u32, error::Error> {
    error::unimp("get effective gid")
}

pub(crate) fn get_current_username() -> Result<String, error::Error> {
    Ok(whoami::username())
}

pub(crate) fn get_all_users() -> Result<Vec<String>, error::Error> {
    // TODO: implement some version of this for Windows
    Ok(vec![])
}

pub(crate) fn get_all_groups() -> Result<Vec<String>, error::Error> {
    // TODO: implement some version of this for Windows
    Ok(vec![])
}
