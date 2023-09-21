use crate::from_lsp::kcl_pos;
use crate::goto_def::goto_definition;
use crate::util::{build_word_index, parse_param_and_compile, Param};
use anyhow;
use kclvm_config::modfile::get_pkg_root;
use lsp_types::Location;
use std::time::{Duration, Instant};

pub(crate) fn find_refs<F: Fn(String) -> Result<(), anyhow::Error>>(
    def_loc: Location,
    name: String,
    cursor_path: String,
    logger: F,
) -> anyhow::Result<Option<Vec<Location>>> {
    // todo: decide the scope by the workspace root and the kcl.mod both, use the narrower scope
    // todo: should use the current file path
    // start timer
    let pkg_root_start = Instant::now();

    if let Some(root) = get_pkg_root(def_loc.uri.path()) {
        // end timer
        let pkg_root_end = Instant::now();
        let pkg_root_duration = (pkg_root_end-pkg_root_start).as_millis();
        println!("get pkg root cost: {}", pkg_root_duration);

        match build_word_index(root) {
            std::result::Result::Ok(word_index) => {
                let word_index_end = Instant::now();
                let word_index_duration = (word_index_end - pkg_root_end).as_millis();
                println!("word index build total cost: {}", word_index_duration);

                if let Some(locs) = word_index.get(name.as_str()).cloned() {
                    println!("matched locations count: {}", locs.len());
                    let result = Some(
                        locs.into_iter()
                            .filter(|ref_loc| {
                                // from location to real def
                                // return if the real def location matches the def_loc
                                let file_path = ref_loc.uri.path().to_string();
                                match parse_param_and_compile(
                                    Param {
                                        file: file_path.clone(),
                                    },
                                    None,
                                ) {
                                    Ok((prog, scope, _)) => {
                                        let ref_pos = kcl_pos(&file_path, ref_loc.range.start);
                                        // find def from the ref_pos
                                        if let Some(real_def) =
                                            goto_definition(&prog, &ref_pos, &scope)
                                        {
                                            match real_def {
                                                lsp_types::GotoDefinitionResponse::Scalar(
                                                    real_def_loc,
                                                ) => real_def_loc == def_loc,
                                                _ => false,
                                            }
                                        } else {
                                            false
                                        }
                                    }
                                    Err(_) => {
                                        let _ = logger(format!("{cursor_path} compilation failed"));
                                        return false;
                                    }
                                }
                            })
                            .collect(),
                    );
                    let compilation_end = Instant::now();
                    let compilation_duration = (compilation_end-word_index_end).as_millis();
                    println!("filter and compilation cost: {}", compilation_duration);
                    return anyhow::Ok(result);
                } else {
                    return Ok(None);
                }
            }
            Err(_) => {
                logger("build word index failed".to_string())?;
                return Ok(None);
            }
        }
    } else {
        return Ok(None);
    }
}

#[cfg(test)]
mod tests {
    use super::find_refs;
    use lsp_types::{Location, Position, Range};
    use std::path::PathBuf;
    use std::time::Instant;

    fn logger(msg: String) -> Result<(), anyhow::Error> {
        println!("{}", msg);
        anyhow::Ok(())
    }

    fn check_locations_match(expect: Vec<Location>, actual: anyhow::Result<Option<Vec<Location>>>) {
        match actual {
            Ok(act) => {
                if let Some(locations) = act {
                    assert_eq!(expect, locations)
                } else {
                    assert!(false, "got empty result. expect: {:?}", expect)
                }
            }
            Err(_) => assert!(false),
        }
    }

    #[test]
    fn find_refs_from_variable_test() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut path = root.clone();
        path.push("src/test_data/find_refs_test/main.k");
        let path = path.to_str().unwrap();
        match lsp_types::Url::from_file_path(path) {
            Ok(url) => {
                let def_loc = Location {
                    uri: url.clone(),
                    range: Range {
                        start: Position::new(0, 0),
                        end: Position::new(0, 1),
                    },
                };
                let expect = vec![
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(0, 0),
                            end: Position::new(0, 1),
                        },
                    },
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(1, 4),
                            end: Position::new(1, 5),
                        },
                    },
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(2, 4),
                            end: Position::new(2, 5),
                        },
                    },
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(12, 14),
                            end: Position::new(12, 15),
                        },
                    },
                ];
                check_locations_match(
                    expect,
                    find_refs(def_loc, "a".to_string(), path.to_string(), logger),
                );
            }
            Err(_) => assert!(false, "file not found"),
        }
    }

    #[test]
    fn find_refs_from_schema_name_test() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut path = root.clone();
        path.push("src/test_data/find_refs_test/main.k");
        let path = path.to_str().unwrap();
        match lsp_types::Url::from_file_path(path) {
            Ok(url) => {
                let def_loc = Location {
                    uri: url.clone(),
                    range: Range {
                        start: Position::new(4, 0),
                        end: Position::new(7, 0),
                    },
                };
                let expect = vec![
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(4, 7),
                            end: Position::new(4, 11),
                        },
                    },
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(8, 7),
                            end: Position::new(8, 11),
                        },
                    },
                    Location {
                        uri: url.clone(),
                        range: Range {
                            start: Position::new(11, 7),
                            end: Position::new(11, 11),
                        },
                    },
                ];
                check_locations_match(
                    expect,
                    find_refs(def_loc, "Name".to_string(), path.to_string(), logger),
                );
            }
            Err(_) => assert!(false, "file not found"),
        }
    }

    #[test]
    fn find_refs_large_test() {
        let p = "/Users/amy/work/Konfig/sigma/base/pkg/kusion_models/app_configuration/sofa/sofa_app_configuration.k";
        let url = lsp_types::Url::from_file_path(p.clone()).unwrap();
        
        let def_loc = Location{
            uri: url.clone(),
            range: Range {
                start: Position::new(17, 7),
                end: Position::new(17, 27),
            },
        };

        // start timer
        let start = Instant::now();

        // Send request and wait for it's response
        find_refs(def_loc, "SofaAppConfiguration".to_string(), p.to_string(), logger);

        // end timer
        let end = Instant::now();
        let duration = (end-start).as_millis();
        println!("total cost:{}\n-------\n", duration);
    }
}
