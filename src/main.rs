use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    path::PathBuf,
    sync::Arc,
};

use anyhow::{anyhow, bail};
use console::style;
use futures_util::StreamExt;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use octocrab::{Octocrab, models::repos::RepoCommit};
use serde::Deserialize;
use tokio::{sync::Mutex, task::JoinSet};

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
    let author = Arc::new(needed_stats.author);
    let needed_languages = Arc::new(needed_stats.languages);
    let needed_experience = needed_stats.experience;
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

    octocrab::initialise(Octocrab::builder().personal_token(pat).build()?);

    let stats: Arc<Mutex<HashMap<String, Stats>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut join_set: JoinSet<Result<(), anyhow::Error>> = JoinSet::new();
    let multi_progress = MultiProgress::new();
    let experience_progress_bar = multi_progress.add(
        ProgressBar::new(needed_experience.len() as u64)
            .with_style(
                ProgressStyle::with_template("{prefix:>12.cyan.bold} [{bar:25}] {pos}/{len}")?
                    .progress_chars("=> "),
            )
            .with_prefix("Compiling"),
    );

    experience_progress_bar.tick();

    for (experience, Experience { repositories }) in needed_experience {
        let author = author.clone();
        let needed_languages = needed_languages.clone();
        let octocrab = octocrab::instance();
        let stats = stats.clone();
        let multi_progress = multi_progress.clone();
        let progress_style = ProgressStyle::with_template(
            "{prefix:>12.cyan.bold} [{bar:25}] {pos}/{len}: {wide_msg}",
        )?
        .progress_chars("=> ");

        join_set.spawn(async move {
            let repository_progress_bar = multi_progress.add(
                ProgressBar::new(repositories.len() as u64)
                    .with_style(progress_style.clone())
                    .with_prefix("Fetching"),
            );

            repository_progress_bar.tick();

            for RepositoryPath { owner, repository } in repositories {
                repository_progress_bar.set_message(format!("{owner}/{repository} ({experience})"));

                let commits = octocrab
                    .repos(owner.clone(), repository.clone())
                    .list_commits()
                    .author(author.deref())
                    .send()
                    .await?
                    .into_stream(&octocrab)
                    .collect::<Vec<Result<RepoCommit, octocrab::Error>>>()
                    .await;
                let commit_handler = octocrab.commits(owner.clone(), repository.clone());
                let commits_progress_bar = multi_progress.add(
                    ProgressBar::new(commits.len() as u64)
                        .with_style(progress_style.clone())
                        .with_prefix("Fetching"),
                );

                for commit in commits {
                    let commit_sha = commit?.sha;

                    commits_progress_bar
                        .set_message(format!("{} ({owner}/{repository})", &commit_sha[..6]));

                    let commit = commit_handler.get(commit_sha).await?;
                    let mut languages = HashSet::new();
                    let mut lines = 0;

                    if let Some(files) = commit.files {
                        for file in files {
                            if let Some(extension) = PathBuf::from(file.filename).extension() {
                                let language = extension.to_string_lossy().to_string();

                                if needed_languages.contains(&language) {
                                    languages.insert(language);
                                }
                            }

                            lines += file.additions;
                        }
                    }

                    stats
                        .lock()
                        .await
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

                    commits_progress_bar.inc(1);
                }

                commits_progress_bar.finish_and_clear();
                repository_progress_bar.inc(1);
                repository_progress_bar.println(format!(
                    "{} {owner}/{repository}",
                    style(format!("{:>12}", "Fetched")).green().bold()
                ));
            }

            repository_progress_bar.finish_and_clear();

            Ok(())
        });
    }

    while let Some(join_result) = join_set.join_next().await {
        join_result??;

        experience_progress_bar.inc(1);
    }

    experience_progress_bar.finish_and_clear();

    println!(
        "{} compiling stats",
        style(format!("{:>12}", "Finished")).green().bold(),
    );

    let stats = stats.lock().await;

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
        println!("{}", style(format!("{experience}:")).green().bold());
        println!(
            "    {} {}",
            style(format!("{:10}", "Languages:")).cyan().bold(),
            languages
                .iter()
                .cloned()
                .collect::<Vec<String>>()
                .join(", ")
        );
        println!(
            "    {} {commits}",
            style(format!("{:10}", "Commits:")).cyan().bold(),
        );
        println!(
            "    {} {lines}",
            style(format!("{:10}", "Lines:")).cyan().bold(),
        );

        if i + 1 != stats.len() {
            println!();
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(e) = try_main().await {
        eprintln!("{}: {e:#}", style("error").red());
    }
}
