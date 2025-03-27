use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use anyhow::{anyhow, bail};
use futures_util::TryStreamExt as _;
use octocrab::Octocrab;
use serde::Deserialize;
use tokio::pin;

#[derive(Deserialize)]
#[serde(try_from = "String")]
struct RepositoryPath {
    owner: String,
    repository: String,
}

impl TryFrom<String> for RepositoryPath {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let (owner, repository) = value.split_once('/').ok_or(anyhow!(
            "expected repository path to be in the format owner/repository"
        ))?;

        Ok(Self {
            owner: owner.to_string(),
            repository: repository.to_string(),
        })
    }
}

#[derive(Deserialize)]
struct Experience {
    repositories: Vec<RepositoryPath>,
}

#[derive(Deserialize)]
struct NeededStats {
    author: String,
    languages: HashSet<String>,
    experience: HashMap<String, Experience>,
}

struct Stats {
    languages: HashSet<String>,
    commits: u64,
    lines: u64,
}

async fn try_main() -> anyhow::Result<()> {
    let stats_toml = tokio::fs::read_to_string("Stats.toml").await?;
    let needed_stats: NeededStats = toml::from_str(&stats_toml)?;
    let keyring_entry = keyring::Entry::new("resume_stats", &whoami::username())?;
    let pat = match keyring_entry.get_password() {
        Ok(pat) => pat,
        Err(e) => {
            if let keyring::Error::NoEntry = e {
                let pat = dialoguer::Password::new()
                    .with_prompt("Please provide a GitHub PAT")
                    .interact()?;

                keyring_entry.set_password(&pat)?;

                pat
            } else {
                bail!(e);
            }
        }
    };
    let octocrab = Octocrab::builder().personal_token(pat).build()?;
    let mut stats: HashMap<String, Stats> = HashMap::new();

    for (experience, Experience { repositories }) in needed_stats.experience {
        println!("Gathering stats for {experience}...");

        for RepositoryPath { owner, repository } in repositories {
            println!("Gathering stats for {owner}/{repository}...");

            let stream = octocrab
                .repos(owner.clone(), repository.clone())
                .list_commits()
                .author(needed_stats.author.clone())
                .send()
                .await?
                .into_stream(&octocrab);
            let commit_handler = octocrab.commits(owner.clone(), repository.clone());

            pin!(stream);

            while let Some(commit) = stream.try_next().await? {
                let commit = commit_handler.get(commit.sha).await?;

                let mut languages = HashSet::new();
                let mut lines = 0;

                if let Some(files) = commit.files {
                    for file in files {
                        if let Some(extension) = PathBuf::from(file.filename).extension() {
                            let language = extension.to_string_lossy().to_string();

                            if needed_stats.languages.contains(&language) {
                                languages.insert(language);
                            }
                        }

                        lines += file.additions;
                    }
                } else {
                    println!("CAN'T SEE COMMIT'S FILES");
                }

                stats
                    .entry(experience.clone())
                    .and_modify(|stats| {
                        for language in languages.clone() {
                            stats.languages.insert(language);
                        }

                        stats.commits += 1;
                        stats.lines += lines;
                    })
                    .or_insert(Stats {
                        languages,
                        commits: 1,
                        lines,
                    });
            }

            println!("Finished gathering stats for {owner}/{repository}");
        }

        println!("Finished gathering stats for {experience}");
    }

    println!();

    for (
        i,
        (
            experience,
            Stats {
                languages,
                commits,
                lines,
            },
        ),
    ) in stats.iter().enumerate()
    {
        println!("{experience}");
        println!(
            "Languages: {}",
            languages
                .iter()
                .cloned()
                .collect::<Vec<String>>()
                .join(", ")
        );
        println!("Commits: {commits}");
        println!("Lines: {lines}");

        if i + 1 != stats.len() {
            println!();
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = try_main().await {
        eprintln!("Error: {e:?}");
    }
}
