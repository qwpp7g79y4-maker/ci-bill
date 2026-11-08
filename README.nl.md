# ci-bill

Zoek uit welke GitHub Actions-workflow het geld kost, en waarom.

English: [README.md](README.md). Handleiding: `man/ci-bill.1`.

`ci-bill` rangschikt de kosten per repository, workflow en runner-SKU en wijst
met `bestand:regel` naar de waarschijnlijke oorzaak. Het leest twee soorten invoer:

- het **detailed usage report** (CSV) uit de GitHub Actions-facturatie, opgegeven met `--usage`
- de runs en jobs van de **Actions REST API**, vooraf opgeslagen door
  `ci-bill fetch` in een map met `runs-*.json`/`jobs-<run_id>*.json`, opgegeven met `--runs`

`report` en `prices` werken offline: geen token, geen netwerk. Alleen `fetch`
gebruikt het netwerk, met een `GH_TOKEN` of `GITHUB_TOKEN` uit de omgeving.

## Installatie

Met Rust 1.85 of nieuwer, vanaf [crates.io](https://crates.io/crates/ci-bill):

```sh
cargo install --locked ci-bill
ci-bill --help
```

Kant-en-klare archieven voor Linux, Windows en macOS staan bij elke
[GitHub-release](https://github.com/qwpp7g79y4-maker/ci-bill/releases), met een
sha256-bestand. Download het archief, controleer de som, pak het uit en zet het
programma op je `PATH`:

```sh
curl -q -fsSL -o ci-bill.tar.gz "$ARCHIVE_URL"
curl -q -fsSL -o ci-bill.tar.gz.sha256 "$ARCHIVE_URL.sha256"
sha256sum ci-bill.tar.gz && cat ci-bill.tar.gz.sha256
tar -xzf ci-bill.tar.gz
install -m 755 ci-bill-*/ci-bill "$HOME/.local/bin/"
```

De tests draaien in CI op Linux, Windows, macOS (Apple Silicon en Intel) en op
Rust 1.85. De suite heeft 536 tests (325 unit, 211 integratie), alle geslaagd.

## Aan de slag in 5 minuten

Vijf opdrachten, offline, op de verzonnen gegevens in `samples/` en `tests/fixtures/api/`. Ze hebben geen token, geen netwerk en geen TUI nodig en lezen geen systeemlogs. Draai ze in de hoofdmap van een kloon, met `ci-bill` op je `PATH` (na `cargo install --locked --path .`, of gebruik `target/release/ci-bill`). Een test draait de opdrachten in dit blok letterlijk.

```sh
ci-bill report --usage samples/usage-sample.csv --workflows samples/.github/workflows --top 2
ci-bill report --usage samples/usage-sample.csv --workflows samples/.github/workflows --md
ci-bill report --usage samples/usage-sample.csv --json
ci-bill report --usage samples/usage-sample.csv --runs tests/fixtures/api
ci-bill prices --label ubuntu-latest
```

1. Het rapport voor de voorbeeld-usage-CSV, met bevindingen uit de voorbeeldworkflows, beperkt tot de top 2 workflows.
2. Hetzelfde als Markdown-teardown van één pagina.
3. Het rapport alleen op usage-gegevens als één JSON-object.
4. Het rapport met de opgeslagen Actions-API-runs en -jobs uit `tests/fixtures/api/`.
5. Het ingebouwde tarief in USD per minuut voor het runnerlabel `ubuntu-latest`, zoals de prijstabel van deze repository het geeft.

Alle vijf geven exit 0. De gegevens gaan alleen over GitHub Actions, net als de rest van deze README. Exporteer voor je eigen account het detailed usage report en geef het door met `--usage`; zie het voorbeeld hieronder.

## Voorbeeld

Bewaar een CSV met één rij (verzonnen gegevens) en draai `ci-bill report`:

```sh
cat > usage.csv <<'CSV'
date,product,sku,quantity,unit_type,applied_cost_per_quantity,gross_amount,discount_amount,net_amount,username,organization,repository,workflow_path
2026-09-01,actions,actions_linux,120,minutes,0.008,0.960,0,0.960,ada,example-org,example-org/sample-app,.github/workflows/ci.yml
CSV
ci-bill report --usage usage.csv
```

```text
USD (from net_amount)
Period: 2026-09-01 .. 2026-09-01
Repositories: example-org/sample-app

Total net: 0.96
Total minutes: 120
Other products: 0.00

Workflow                                          Net  Gross  Discount  Minutes    Share
example-org/sample-app .github/workflows/ci.yml  0.96   0.96      0.00      120  100.00%
  actions_linux                                  0.96   0.96      0.00      120         
  measured 0.00  invoice 0.96  difference 0.96
```

Meer verzonnen invoer met de verwachte uitvoer staat in `examples/`.

## Opties

Elke optie uit `--help`, per commando.

`ci-bill` (hoofdniveau):

- `-h`, `--help`: toon de hulptekst.
- `-V`, `--version`: toon de versie.
- `--quiet`: verberg de `what to do`-regel, ook op een terminal. Geen contextregel. Geldt ook voor `report`, `prices` en `fetch`.
- `--verbose`: toon de `what to do`-regel, en bij succes één `context:`-regel op stderr, ook in een pipe. Geldt ook voor `report`, `prices` en `fetch`.

`ci-bill report`:

- `--usage <CSV>`: detailed usage report als CSV (verplicht). `-` leest standaardinvoer.
- `--workflows <DIR>`: map met workflow-YAML om op bevindingen te scannen.
- `--runs <DIR>`: opgeslagen Actions API-JSON en gecachete externe workflows, zoals `ci-bill fetch` ze schrijft.
- `--top <N>`: toon de N workflows met het hoogste netbedrag.
- `--json`: print één JSON-object in plaats van het tekstrapport.
- `--budget <AMOUNT>`: budget voor het netto totaal in USD; exitcode 1 als het totaal hoger is.
- `--fix-diff`: print een unified diff die de bevinding met de hoogste besparing herstelt.
- `--md`: print een Markdown-analyse van één pagina in plaats van het rapport.
- `--output <PATH>`: schrijf het rapport naar dit bestand in plaats van standaarduitvoer. Een bestaand bestand wordt overschreven. Een ontbrekende bovenliggende map is exitcode 1 en wordt niet aangemaakt.
- `-h`, `--help`: toon de hulptekst.

`ci-bill prices`:

- `--json`: print één JSON-object in plaats van de teksttabel.
- `--runner-map <CSV>`: CSV met `label,sku`-rijen die de ingebouwde koppeling vervangen. `-` leest standaardinvoer.
- `--label <LABEL>`: runnerlabel of lijst, bijvoorbeeld `ubuntu-latest`.
- `--sku <SKU>`: Actions-SKU. Samen met `--label` moet het de SKU zijn waar het label naar verwijst.
- `--usage <CSV>`: usage report waarvan de eigen rijen het tabeltarief vervangen. `-` leest standaardinvoer.
- `--repository <OWNER/REPO>`: repository die de usage-rijen bezit.
- `--workflow <PATH>`: workflowpad in het usage report.
- `--output <PATH>`: schrijf het rapport naar dit bestand in plaats van standaarduitvoer. Een bestaand bestand wordt overschreven. Een ontbrekende bovenliggende map is exitcode 1 en wordt niet aangemaakt.
- `-h`, `--help`: toon de hulptekst.

`ci-bill fetch`:

- `--repo <OWNER/REPO>`: te lezen repository (verplicht).
- `--out <DIR>`: map voor de opgeslagen bestanden (verplicht).
- `--since <DATE>`: eerste dag, `YYYY-MM-DD` in UTC (verplicht).
- `--until <DATE>`: laatste dag, `YYYY-MM-DD` in UTC. Standaard vandaag.
- `-h`, `--help`: toon de hulptekst.

## Afsluitcodes

| Code | Betekenis |
| ---- | --------- |
| 0 | goed |
| 1 | `--budget` is overschreden, of het rapport was niet te schrijven (stdout of `--output`, bijvoorbeeld schijf vol of een ontbrekende bovenliggende map) |
| 2 | een invoer was onbruikbaar: ontbrekende kolom, kapot getal, ongeldige YAML/JSON, onleesbaar bestand, onbekende SKU, of `fetch` kon de GitHub API niet gebruiken |
| 3 | verkeerd gebruik: onbekende optie, ontbrekende verplichte optie, ongeldig `--budget`-bedrag, runnerlabel niet in de prijstabel, of `fetch`-gebruik zoals een ontbrekend token of `--until` vóór `--since` |

Bij een fout noemt `ci-bill` wat er misging en, bij invoerfouten, onder
"what to do" wat je eraan kunt doen.

## Licentie

MIT OR Apache-2.0. Onderhouden door PEPAI (pep_ai@icloud.com).
