use clap::Parser;
use kr2r::compact_hash::CompactHashTable;
use kr2r::iclassify::{classify_seq, mask_low_quality_bases};
use kr2r::mmscanner::MinimizerScanner;
// use kr2r::readcounts::TaxonCounters;
use kr2r::pair;
use kr2r::taxonomy::Taxonomy;
use kr2r::IndexOptions;
use rayon::prelude::*;
use seq_io::fastq::{Reader as FqReader, Record, RefRecord};
use seq_io::parallel::read_parallel;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::io::{Error, ErrorKind, Result};
// use std::sync::Mutex;
// use std::time::Duration;

/// Command line arguments for the classify program.
///
/// This structure defines the command line arguments that are accepted by the classify program.
/// It uses the `clap` crate for parsing command line arguments.
#[derive(Parser, Debug, Clone)]
#[clap(
    version,
    about = "classify",
    long_about = "classify a set of sequences"
)]
struct Args {
    /// The file path for the Kraken 2 index.
    #[clap(short = 'H', long = "index-filename", value_parser, required = true)]
    index_filename: String,

    /// The file path for the Kraken 2 taxonomy.
    #[clap(short = 't', long = "taxonomy-filename", value_parser, required = true)]
    taxonomy_filename: String,

    /// The file path for the Kraken 2 options.
    #[clap(short = 'o', long = "options-filename", value_parser, required = true)]
    options_filename: String,

    /// Confidence score threshold, default is 0.0.
    #[clap(
        short = 'T',
        long = "confidence-threshold",
        value_parser,
        default_value_t = 0.0
    )]
    confidence_threshold: f64,

    /// Enable quick mode for faster processing.
    #[clap(short = 'q', long = "quick-mode", action)]
    quick_mode: bool,

    /// The number of threads to use, default is 1.
    #[clap(short = 'p', long = "num-threads", value_parser, default_value_t = 1)]
    num_threads: i32,

    /// The minimum number of hit groups needed for a call.
    #[clap(
        short = 'g',
        long = "minimum-hit-groups",
        value_parser,
        default_value_t = 2
    )]
    minimum_hit_groups: i32,

    /// Enable paired-end processing.
    #[clap(short = 'P', long = "paired-end-processing", action)]
    paired_end_processing: bool,

    /// Process pairs with mates in the same file.
    #[clap(short = 'S', long = "single-file-pairs", action)]
    single_file_pairs: bool,

    /// Use mpa-style report format.
    #[clap(short = 'm', long = "mpa-style-report", action)]
    mpa_style_report: bool,

    /// Report k-mer data in the output.
    #[clap(short = 'K', long = "report-kmer-data", action)]
    report_kmer_data: bool,

    /// File path for outputting the report.
    #[clap(short = 'R', long = "report-filename", value_parser)]
    report_filename: Option<String>,

    /// Report taxa with zero count.
    #[clap(short = 'z', long = "report-zero-counts", action)]
    report_zero_counts: bool,

    /// File path for outputting classified sequences.
    #[clap(short = 'C', long = "classified-output-filename", value_parser)]
    classified_output_filename: Option<String>,

    /// File path for outputting unclassified sequences.
    #[clap(short = 'U', long = "unclassified-output-filename", value_parser)]
    unclassified_output_filename: Option<String>,

    /// File path for outputting normal Kraken output.
    #[clap(short = 'O', long = "kraken-output-filename", value_parser)]
    kraken_output_filename: Option<String>,

    /// Print scientific name instead of taxid in Kraken output.
    #[clap(short = 'n', long = "print-scientific-name", action)]
    print_scientific_name: bool,

    /// Minimum quality score for FASTQ data, default is 0.
    #[clap(
        short = 'Q',
        long = "minimum-quality-score",
        value_parser,
        default_value_t = 0
    )]
    minimum_quality_score: i32,

    /// Use memory mapping to access hash and taxonomy data.
    #[clap(short = 'M', long = "use-memory-mapping", action)]
    use_memory_mapping: bool,

    /// Input files for processing.
    ///
    /// A list of input file paths (FASTA/FASTQ) to be processed by the classify program.
    // #[clap(short = 'F', long = "files")]
    input_files: Vec<String>,
}

fn check_feature(dna_db: bool) -> Result<()> {
    #[cfg(feature = "dna")]
    if !dna_db {
        return Err(Error::new(
            ErrorKind::Other,
            "Feature 'dna' is enabled but 'dna_db' is false",
        ));
    }

    #[cfg(feature = "protein")]
    if dna_db {
        return Err(Error::new(
            ErrorKind::Other,
            "Feature 'protein' is enabled but 'dna_db' is true",
        ));
    }

    Ok(())
}

fn get_record_id(ref_record: &RefRecord) -> String {
    std::str::from_utf8(ref_record.head().split(|b| *b == b' ').next().unwrap())
        .unwrap_or_default()
        .into()
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord)]
struct SeqReads {
    pub dna_id: String,
    pub seq_paired: Vec<Vec<u8>>,
}

/// 处理fastq文件
fn process_files(
    args: Args,
    idx_opts: IndexOptions,
    cht: &CompactHashTable<u32>,
    taxonomy: &Taxonomy,
    writer: &mut Box<dyn std::io::Write>,
) {
    let queue_len = if args.num_threads > 2 {
        args.num_threads as usize - 2
    } else {
        1
    };
    let meros = idx_opts.as_meros();

    if args.paired_end_processing && !args.single_file_pairs {
        // 处理成对的文件
        for file_pair in args.input_files.chunks(2) {
            let file1 = &file_pair[0];
            let file2 = &file_pair[1];
            // 对 file1 和 file2 执行分类处理
            let pair_reader = pair::PairReader::from_path(file1, file2).unwrap();
            read_parallel(
                pair_reader,
                args.num_threads as u32,
                queue_len,
                |record_set| {
                    let mut seq_pair_set = HashSet::<SeqReads>::new();

                    for records in record_set.into_iter() {
                        let dna_id = get_record_id(&records.0);
                        let seq1 = mask_low_quality_bases(&records.0, args.minimum_quality_score);
                        let seq2 = mask_low_quality_bases(&records.1, args.minimum_quality_score);
                        let seq_paired: Vec<Vec<u8>> = vec![seq1, seq2];
                        seq_pair_set.insert(SeqReads { dna_id, seq_paired });
                    }
                    seq_pair_set
                },
                |record_sets| {
                    while let Some(Ok((_, seq_pair_set))) = record_sets.next() {
                        let results: Vec<String> = seq_pair_set
                            .into_par_iter()
                            .map(|item| {
                                let mut scanner = MinimizerScanner::new(idx_opts.as_meros());
                                classify_seq(
                                    &taxonomy,
                                    &cht,
                                    &mut scanner,
                                    &item.seq_paired,
                                    meros,
                                    args.confidence_threshold,
                                    args.minimum_hit_groups,
                                    item.dna_id,
                                )
                            })
                            .collect();
                        for result in results {
                            writeln!(writer, "{}", result).expect("Unable to write to file");
                        }
                    }
                },
            )
        }
    } else {
        for file in args.input_files {
            // 对 file 执行分类处理
            let reader = FqReader::from_path(file).unwrap();
            read_parallel(
                reader,
                args.num_threads as u32,
                queue_len,
                |record_set| {
                    let mut seq_pair_set = HashSet::<SeqReads>::new();

                    for records in record_set.into_iter() {
                        let dna_id = get_record_id(&records);
                        let seq1 = mask_low_quality_bases(&records, args.minimum_quality_score);
                        let seq_paired: Vec<Vec<u8>> = vec![seq1];
                        seq_pair_set.insert(SeqReads { dna_id, seq_paired });
                    }
                    seq_pair_set
                },
                |record_sets| {
                    while let Some(Ok((_, seq_pair_set))) = record_sets.next() {
                        let results: Vec<String> = seq_pair_set
                            .into_par_iter()
                            .map(|item| {
                                let mut scanner = MinimizerScanner::new(idx_opts.as_meros());
                                classify_seq(
                                    &taxonomy,
                                    &cht,
                                    &mut scanner,
                                    &item.seq_paired,
                                    meros,
                                    args.confidence_threshold,
                                    args.minimum_hit_groups,
                                    item.dna_id,
                                )
                            })
                            .collect();
                        for result in results {
                            writeln!(writer, "{}", result).expect("Unable to write to file");
                        }
                    }
                },
            )
            // read_parallel(
            //     reader,
            //     args.num_threads as u32,
            //     queue_len,
            //     |record_set| {
            //         let mut scanner = MinimizerScanner::new(idx_opts.as_meros());
            //         for record1 in record_set.into_iter() {
            //             let dna_id = get_record_id(&record1);
            //             let record_list = vec![record1];
            //             classify_seq(
            //                 &taxonomy,
            //                 &cht,
            //                 &mut scanner,
            //                 &record_list,
            //                 args.minimum_quality_score,
            //                 meros,
            //                 args.confidence_threshold,
            //                 args.minimum_hit_groups,
            //                 dna_id.into(),
            //             );
            //         }
            //     },
            //     |_| {
            //         // while let Some(Ok((_, _))) = record_sets.next() {}
            //     },
            // )
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let idx_opts = IndexOptions::read_index_options(args.options_filename.clone())?;
    check_feature(idx_opts.dna_db)?;
    let taxo = Taxonomy::from_file(&args.taxonomy_filename)?;
    let cht = CompactHashTable::from(args.index_filename.clone())?;

    if args.paired_end_processing && !args.single_file_pairs && args.input_files.len() % 2 != 0 {
        // 验证文件列表是否为偶数个
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Paired-end processing requires an even number of input files.",
        ));
    }

    // let file = File::create("out_rust.txt")?;
    // let mut writer = BufWriter::new(file);

    let mut writer: Box<dyn Write> = match &args.kraken_output_filename {
        Some(filename) => {
            let file = File::create(filename)?;
            Box::new(BufWriter::new(file)) as Box<dyn Write>
        }
        None => Box::new(io::stdout()) as Box<dyn Write>,
    };

    process_files(args, idx_opts, &cht, &taxo, &mut writer);
    Ok(())
}