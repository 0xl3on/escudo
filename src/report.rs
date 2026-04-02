use colored::Colorize;
use serde::Serialize;

use crate::error::{ReportError, Violation};
use crate::policy::PolicyViolation;

/// Output format for the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Human-readable with ANSI colors (default).
    Human,
    /// Machine-readable JSON.
    Json,
    /// GitHub Actions workflow commands (`::warning::`, `::error::`).
    Github,
    /// Self-contained HTML file.
    Html,
}

/// The complete result of an escudo audit run.
#[derive(Debug, Clone)]
pub struct EscudoReport {
    pub violations: Vec<Violation>,
    pub packages_checked: usize,
}

impl EscudoReport {
    /// Returns `true` when the audit found no violations.
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }

    /// Print the report in the requested format.
    /// For HTML, writes a file and returns the path.
    pub fn print(&self, format: OutputFormat) -> Result<Option<String>, ReportError> {
        match format {
            OutputFormat::Human => {
                self.print_human();
                Ok(None)
            }
            OutputFormat::Json => {
                self.print_json()?;
                Ok(None)
            }
            OutputFormat::Github => {
                self.print_github();
                Ok(None)
            }
            OutputFormat::Html => {
                let path = self.write_html()?;
                Ok(Some(path))
            }
        }
    }

    /// Print a human-readable summary to stdout.
    fn print_human(&self) {
        println!();
        println!("{}", "━━━ escudo supply chain audit ━━━".bold());
        println!(
            "  packages checked: {}",
            self.packages_checked.to_string().cyan()
        );
        println!();

        if self.is_clean() {
            println!("  {} No violations found.", "OK".green().bold());
            println!();
            return;
        }

        let unverified_count = self
            .violations
            .iter()
            .filter(|v| matches!(v, Violation::Unverified { .. }))
            .count();
        let freshness_count = self
            .violations
            .iter()
            .filter(|v| matches!(v, Violation::Freshness(_)))
            .count();
        let policy_count = self
            .violations
            .iter()
            .filter(|v| matches!(v, Violation::Policy(_)))
            .count();

        if unverified_count > 0 {
            println!(
                "  {} Unverified crates ({}):",
                "FAIL".red().bold(),
                unverified_count,
            );
            for v in &self.violations {
                if let Violation::Unverified {
                    crate_name,
                    version,
                } = v
                {
                    println!(
                        "    {} {} v{} — not found in crates.io API",
                        "-".red(),
                        crate_name.bold(),
                        version,
                    );
                }
            }
            println!();
        }

        if freshness_count > 0 {
            println!(
                "  {} Freshness violations ({}):",
                "WARN".yellow().bold(),
                freshness_count,
            );
            for v in &self.violations {
                if let Violation::Freshness(f) = v {
                    println!(
                        "    {} {} v{} — published {} days ago (cooldown: {})",
                        "-".red(),
                        f.crate_name.bold(),
                        f.version,
                        f.age_days,
                        f.cooldown_days,
                    );
                }
            }
            println!();
        }

        if policy_count > 0 {
            println!(
                "  {} Policy violations ({}):",
                "FAIL".red().bold(),
                policy_count,
            );
            for v in &self.violations {
                if let Violation::Policy(p) = v {
                    println!("    {} {}", "-".red(), p);
                }
            }
            println!();
        }

        println!(
            "  {} {} violation(s) found.",
            "RESULT".red().bold(),
            self.violations.len(),
        );
        println!();
    }

    /// Serialize the report as JSON and print to stdout.
    fn print_json(&self) -> Result<(), ReportError> {
        let json_report = JsonReport::from(self);
        let output = serde_json::to_string_pretty(&json_report)?;
        println!("{}", output);
        Ok(())
    }

    /// Print GitHub Actions workflow commands.
    fn print_github(&self) {
        for v in &self.violations {
            match v {
                Violation::Freshness(f) => {
                    println!(
                        "::warning title=escudo: fresh crate::{} v{} published {} days ago (cooldown: {} days)",
                        f.crate_name, f.version, f.age_days, f.cooldown_days,
                    );
                }
                Violation::Policy(p) => {
                    println!("::error title=escudo: policy violation::{}", p);
                }
                Violation::Unverified {
                    crate_name,
                    version,
                } => {
                    println!(
                        "::error title=escudo: unverified crate::{} v{} not found in crates.io API",
                        crate_name, version,
                    );
                }
            }
        }

        if self.is_clean() {
            println!(
                "::notice title=escudo::All {} packages passed supply chain checks.",
                self.packages_checked
            );
        }
    }

    /// Render the report as HTML and write to a file. Returns the file path.
    fn write_html(&self) -> Result<String, ReportError> {
        let template = include_str!("template.html.handlebars");
        let hbs = handlebars::Handlebars::new();

        let data = HtmlTemplateData::from(self);
        let html = hbs.render_template(template, &data)?;

        // Write to XDG data dir (e.g. ~/.local/share/escudo/reports/)
        let report_dir = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("escudo")
            .join("reports");
        std::fs::create_dir_all(&report_dir)?;

        let filename = format!(
            "escudo-report-{}.html",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let path = report_dir.join(&filename);
        std::fs::write(&path, &html)?;

        Ok(path.display().to_string())
    }
}

#[derive(Serialize)]
struct UnverifiedEntry {
    crate_name: String,
    version: String,
}

#[derive(Serialize)]
struct HtmlTemplateData {
    date: String,
    project_name: String,
    version: String,
    packages_checked: usize,
    violations_count: usize,
    violations_plural: bool,
    clean: bool,
    unverified_count: usize,
    freshness_count: usize,
    policy_count: usize,
    unverified_crates: Vec<UnverifiedEntry>,
    freshness_violations: Vec<HtmlFreshnessViolation>,
    policy_violations: Vec<HtmlPolicyViolation>,
}

#[derive(Serialize)]
struct HtmlFreshnessViolation {
    crate_name: String,
    version: String,
    published: String,
    age_days: i64,
    cooldown_days: u32,
}

#[derive(Serialize)]
struct HtmlPolicyViolation {
    kind: String,
    message: String,
}

impl From<&EscudoReport> for HtmlTemplateData {
    fn from(report: &EscudoReport) -> Self {
        let mut unverified = Vec::new();
        let mut freshness = Vec::new();
        let mut policy = Vec::new();

        for v in &report.violations {
            match v {
                Violation::Unverified {
                    crate_name,
                    version,
                } => unverified.push(UnverifiedEntry {
                    crate_name: crate_name.clone(),
                    version: version.clone(),
                }),
                Violation::Freshness(f) => freshness.push(HtmlFreshnessViolation {
                    crate_name: f.crate_name.clone(),
                    version: f.version.clone(),
                    published: f.published.format("%Y-%m-%d %H:%M UTC").to_string(),
                    age_days: f.age_days,
                    cooldown_days: f.cooldown_days,
                }),
                Violation::Policy(p) => policy.push(HtmlPolicyViolation {
                    kind: policy_kind_label(p),
                    message: p.to_string(),
                }),
            }
        }

        let project_name = std::env::current_dir()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_else(|| "unknown".to_string());

        HtmlTemplateData {
            date: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
            project_name,
            version: env!("CARGO_PKG_VERSION").to_string(),
            packages_checked: report.packages_checked,
            violations_count: report.violations.len(),
            violations_plural: report.violations.len() != 1,
            clean: report.is_clean(),
            unverified_count: unverified.len(),
            freshness_count: freshness.len(),
            policy_count: policy.len(),
            unverified_crates: unverified,
            freshness_violations: freshness,
            policy_violations: policy,
        }
    }
}

#[derive(Serialize)]
struct JsonReport {
    packages_checked: usize,
    violations_count: usize,
    clean: bool,
    unverified_crates: Vec<UnverifiedEntry>,
    freshness_violations: Vec<JsonFreshnessViolation>,
    policy_violations: Vec<JsonPolicyViolation>,
}

#[derive(Serialize)]
struct JsonFreshnessViolation {
    crate_name: String,
    version: String,
    published: String,
    age_days: i64,
    cooldown_days: u32,
}

#[derive(Serialize)]
struct JsonPolicyViolation {
    kind: String,
    message: String,
}

impl From<&EscudoReport> for JsonReport {
    fn from(report: &EscudoReport) -> Self {
        let mut unverified = Vec::new();
        let mut freshness = Vec::new();
        let mut policy = Vec::new();

        for v in &report.violations {
            match v {
                Violation::Unverified {
                    crate_name,
                    version,
                } => unverified.push(UnverifiedEntry {
                    crate_name: crate_name.clone(),
                    version: version.clone(),
                }),
                Violation::Freshness(f) => freshness.push(JsonFreshnessViolation {
                    crate_name: f.crate_name.clone(),
                    version: f.version.clone(),
                    published: f.published.to_rfc3339(),
                    age_days: f.age_days,
                    cooldown_days: f.cooldown_days,
                }),
                Violation::Policy(p) => policy.push(JsonPolicyViolation {
                    kind: policy_kind(p),
                    message: p.to_string(),
                }),
            }
        }

        JsonReport {
            packages_checked: report.packages_checked,
            violations_count: report.violations.len(),
            clean: report.is_clean(),
            unverified_crates: unverified,
            freshness_violations: freshness,
            policy_violations: policy,
        }
    }
}

fn policy_kind(v: &PolicyViolation) -> String {
    match v {
        PolicyViolation::GitDependency { .. } => "git_dependency",
        PolicyViolation::PathDependency { .. } => "path_dependency",
        PolicyViolation::DuplicateVersions { .. } => "duplicate_versions",
        PolicyViolation::BannedCrate { .. } => "banned_crate",
        PolicyViolation::DisallowedLicense { .. } => "disallowed_license",
    }
    .to_string()
}

fn policy_kind_label(v: &PolicyViolation) -> String {
    match v {
        PolicyViolation::GitDependency { .. } => "git dep",
        PolicyViolation::PathDependency { .. } => "path dep",
        PolicyViolation::DuplicateVersions { .. } => "duplicates",
        PolicyViolation::BannedCrate { .. } => "banned",
        PolicyViolation::DisallowedLicense { .. } => "license",
    }
    .to_string()
}
