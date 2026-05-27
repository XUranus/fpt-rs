use clap::Parser;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(long)]
    target: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    fpt::logging::init(2);

    let location = fpt::smb::SmbLocation::from_url(&cli.target)?;
    println!("target={}", location.display_string());
    println!("share_unc={}", location.share_unc_path()?);
    println!("root_unc={}", location.root_unc_path()?);

    let client = smb_client::Client::new(fpt::smb::client_config(&location));
    let share_root = location.share_unc_path()?;
    let username = location.username.clone().unwrap_or_default();
    let password = location.password.clone().unwrap_or_default();

    println!("step=connect");
    let conn = client.connect(&location.host).await?;
    println!("step=connect ok");

    let identity = sspi::AuthIdentity {
        username: sspi::Username::parse(&username)?,
        password: sspi::Secret::from(password),
    };

    println!("step=authenticate");
    let session = conn.authenticate(identity).await?;
    println!("step=authenticate ok");

    println!("step=tree_connect");
    let tree = session.tree_connect(&share_root).await?;
    println!("step=tree_connect ok");

    if !location.sub_path.is_empty() {
        println!("step=open_root");
        let access = smb_client::FileAccessMask::new().with_generic_read(true);
        let resource = tree
            .open_existing(location.root_unc_path()?.path().unwrap_or(""), access)
            .await?;
        match resource {
            smb_client::Resource::Directory(dir) => {
                dir.close().await?;
            }
            smb_client::Resource::File(file) => {
                file.close().await?;
            }
            smb_client::Resource::Pipe(pipe) => {
                pipe.close().await?;
            }
        }
        println!("step=open_root ok");
    }

    tree.disconnect().await?;
    session.logoff().await?;
    client.close().await?;
    Ok(())
}
