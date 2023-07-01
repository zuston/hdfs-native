pub(crate) mod minidfs;

use hdfs_native::object_store::HdfsObjectStore;
use hdfs_native::{client::Client, Result};
use std::collections::HashSet;
use std::env;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::Command;
use tempfile::NamedTempFile;
use which::which;

use crate::common::minidfs::MiniDfs;

use self::minidfs::DfsFeatures;

const TEST_FILE_INTS: usize = 64 * 1024 * 1024;

fn setup(features: &HashSet<DfsFeatures>) -> MiniDfs {
    let hadoop_exc = which("hadoop").expect("Failed to find hadoop executable");

    let dfs = MiniDfs::with_features(features);

    env::set_var("HADOOP_CONF_DIR", "target/test");

    if features.contains(&DfsFeatures::SECURITY) {
        let kdestroy_exec = which("kdestroy").expect("Failed to find kdestroy executable");
        Command::new(kdestroy_exec).spawn().unwrap().wait().unwrap();

        if !PathBuf::from("target/test/hdfs.keytab").exists() {
            panic!("Failed to find keytab");
        }

        let krb_conf = dfs.krb_conf.as_ref().unwrap();

        if !PathBuf::from(krb_conf).exists() {
            panic!("Failed to find krb5.conf");
        }

        env::set_var("KRB5_CONFIG", krb_conf);
        env::set_var(
            "HADOOP_OPTS",
            &format!("-Djava.security.krb5.conf={}", krb_conf),
        );
    }

    // If we testing token auth, set the path to the file and make sure we don't have an old kinit, otherwise kinit
    if features.contains(&DfsFeatures::TOKEN) {
        env::set_var("HADOOP_TOKEN_FILE_LOCATION", "target/test/delegation_token");
    } else {
        let kinit_exec = which("kinit").expect("Failed to find kinit executable");
        Command::new(kinit_exec)
            .args(["-kt", "target/test/hdfs.keytab", "hdfs/localhost"])
            .spawn()
            .unwrap()
            .wait()
            .unwrap();
    }

    let mut file = NamedTempFile::new_in("target/test").unwrap();
    {
        let mut writer = BufWriter::new(file.as_file_mut());
        for i in 0..TEST_FILE_INTS as i32 {
            let bytes = i.to_be_bytes();
            writer.write(&bytes).unwrap();
        }
        writer.flush().unwrap();
    }

    let mut cmd = Command::new(hadoop_exc)
        .args([
            "fs",
            "-copyFromLocal",
            "-f",
            file.path().to_str().unwrap(),
            &format!("{}/testfile", dfs.url),
        ])
        .spawn()
        .unwrap();
    assert!(cmd.wait().unwrap().success());

    dfs
}

pub(crate) async fn test_with_features(features: &HashSet<DfsFeatures>) -> Result<()> {
    let _ = env_logger::builder().is_test(true).try_init();

    let _dfs = setup(features);
    let client = Client::new(&_dfs.url)?;

    test_listing(&client).await?;
    test_read(&client).await?;
    test_rename(&client).await?;

    #[cfg(feature = "object_store")]
    test_object_store(client).await.unwrap();

    Ok(())
}

async fn test_listing(client: &Client) -> Result<()> {
    let statuses = client.list_status("/").await?;
    assert_eq!(statuses.len(), 1);
    let status = &statuses[0];
    assert_eq!(status.path, "testfile");
    assert_eq!(status.length, TEST_FILE_INTS * 4);
    Ok(())
}

async fn test_read(client: &Client) -> Result<()> {
    // Read the whole file
    let reader = client.read("/testfile").await?;
    let buf = reader.read(0, TEST_FILE_INTS * 4).await?;
    for i in 0..TEST_FILE_INTS as i32 {
        let mut dst = [0u8; 4];
        let offset = (i * 4) as usize;
        dst.copy_from_slice(&buf.slice(offset..offset + 4)[..]);
        assert_eq!(i32::from_be_bytes(dst), i);
    }

    // Read a single integer from the file
    let buf = reader.read(TEST_FILE_INTS as usize / 2 * 4, 4).await?;
    let mut dst = [0u8; 4];
    dst.copy_from_slice(&buf[..]);
    assert_eq!(i32::from_be_bytes(dst), TEST_FILE_INTS as i32 / 2);
    Ok(())
}

async fn test_rename(client: &Client) -> Result<()> {
    client.rename("/testfile", "/testfile2", false).await?;

    assert!(client.list_status("/testfile").await.is_err());
    assert_eq!(client.list_status("/testfile2").await?.len(), 1);

    client.rename("/testfile2", "/testfile", false).await?;
    assert!(client.list_status("/testfile2").await.is_err());
    assert_eq!(client.list_status("/testfile").await?.len(), 1);

    Ok(())
}

#[cfg(feature = "object_store")]
async fn test_object_store(client: Client) -> object_store::Result<()> {
    use object_store::{path::Path, ObjectStore};

    let store = HdfsObjectStore::new(client);

    store
        .rename(&Path::from("/testfile"), &Path::from("/testfile2"))
        .await?;

    Ok(())
}
