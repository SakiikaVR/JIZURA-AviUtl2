fn main(){let path=std::env::args().nth(1).expect("usage: audit <dist>");if let Err(e)=jizura_aviutl2::audit_test(std::path::Path::new(&path)){eprintln!("{e:#}");std::process::exit(1);}}
