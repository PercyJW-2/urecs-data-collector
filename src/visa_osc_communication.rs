use std::ffi::CString;
use visa_rs::{AsResourceManager, DefaultRM};
use anyhow::Result;

pub(crate) fn setup_osc() -> Result<()> {
    let rm = DefaultRM::new()?;
    let mut list = rm.find_res_list(&CString::new("?*INSTR")?.into())?;
    Ok(())
}