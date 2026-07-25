/// Integration tests for the CFG module.
///
/// We parse real .class files (from the testData directory) and verify
/// that the CFG is structurally sound: every block reachable from ENTRY
/// has the correct predecessor/successor invariants.

#[cfg(test)]
mod cfg_tests {
    use abyssflower_lib::cfg::{builder, DomTree, ENTRY_BLOCK, EXIT_BLOCK};
    use abyssflower_lib::classfile::attribute::Attribute;
    use abyssflower_lib::ClassFile;

    /// Load a test .class file, returning its bytes.
    fn load_class(name: &str) -> Vec<u8> {
        let path = format!(
            "vineflower-master/testData/classes/custom/{}",
            name
        );
        std::fs::read(&path)
            .unwrap_or_else(|_| panic!("Cannot read test class: {}", path))
    }

    /// Build CFGs for every method in a class file and run sanity checks.
    fn check_class(bytes: &[u8]) {
        let cf = ClassFile::parse(bytes).expect("parse failed");

        for method in &cf.methods {
            let code = match method.code() {
                Some(c) => c,
                None => continue,
            };

            let cfg = builder::build(code);

            // 1. ENTRY and EXIT always exist
            let _ = cfg.entry();
            let _ = cfg.exit();

            // 2. Every block referenced in a succ/pred list actually exists
            for block in cfg.blocks.iter() {
                for &succ in &block.succs {
                    assert!(
                        cfg.blocks.iter().any(|b| b.id == succ),
                        "succ {} of block {} not found in CFG for {}.{}",
                        succ, block.id, cf.this_class, method.name
                    );
                }
                for &pred in &block.preds {
                    assert!(
                        cfg.blocks.iter().any(|b| b.id == pred),
                        "pred {} of block {} not found in CFG for {}.{}",
                        pred, block.id, cf.this_class, method.name
                    );
                }
            }

            // 3. Pred/succ symmetry: if A → B then B has A in preds
            for block in cfg.blocks.iter() {
                for &succ in &block.succs {
                    let succ_block = cfg.block(succ);
                    assert!(
                        succ_block.preds.contains(&block.id),
                        "symmetry broken: {}->{} but {} missing from preds of {}",
                        block.id, succ, block.id, succ
                    );
                }
            }

            // 4. Instruction count matches what was parsed
            let total_insns: usize = cfg.blocks.iter()
                .map(|b| b.instructions.len())
                .sum();
            assert_eq!(
                total_insns,
                code.instructions.len(),
                "instruction count mismatch for {}.{}",
                cf.this_class, method.name
            );

            // 5. Dominator tree is computable without panic
            if cfg.blocks.len() > 2 {
                let dom = DomTree::compute(&cfg);
                // ENTRY dominates itself
                assert!(dom.dominates(ENTRY_BLOCK, ENTRY_BLOCK));
                // ENTRY dominates every reachable block
                for id in cfg.rpo() {
                    assert!(
                        dom.dominates(ENTRY_BLOCK, id),
                        "ENTRY does not dominate block {} in {}.{}",
                        id, cf.this_class, method.name
                    );
                }
            }
        }
    }

    #[test]
    fn test_cfg_main_class() {
        check_class(&load_class("../bulk/pkg/Main.class"));
    }

    #[test]
    fn test_cfg_switch_enum() {
        check_class(&load_class("TestEclipseSwitchEnum.class"));
    }

    #[test]
    fn test_cfg_switch_string() {
        check_class(&load_class("TestEclipseSwitchString.class"));
    }

    #[test]
    fn test_cfg_jsr() {
        check_class(&load_class("TestJsr.class"));
    }

    #[test]
    fn test_cfg_jsr2() {
        check_class(&load_class("TestJsr2.class"));
    }

    #[test]
    fn test_cfg_hotjava() {
        check_class(&load_class("TestHotjava.class"));
    }

    #[test]
    fn test_cfg_java1_synchronized() {
        check_class(&load_class("TestJava1Synchronized.class"));
    }

    #[test]
    fn test_cfg_inner_constructor() {
        let bytes = std::fs::read(
            "vineflower-master/testData/classes/custom/v11/TestInnerClassConstructor.class"
        ).expect("cannot read");
        check_class(&bytes);
    }

    #[test]
    fn test_cfg_string_concat_j19() {
        check_class(&load_class("TestStringConcatJ19.class"));
    }

    #[test]
    fn test_cfg_switch_enum_j21() {
        check_class(&load_class("TestSwitchOnEnumWithoutEnumJ21.class"));
    }

    // ── manual CFG shape tests ─────────────────────────────────────────

    /// Simple straight-line method: `() -> void { return; }`
    /// Expected CFG: ENTRY → B1(return) → EXIT
    #[test]
    fn test_cfg_straight_line() {
        use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};
        use abyssflower_lib::classfile::attribute::{CodeAttribute, ExceptionHandler};

        let code = CodeAttribute {
            max_stack:       1,
            max_locals:      1,
            instructions:    vec![
                Instruction { offset: 0, opcode: 0xb1 /*return*/, wide: false, kind: InsnKind::NoOperand },
            ],
            exception_table: vec![],
            attributes:      vec![],
        };

        let cfg = builder::build(&code);
        // ENTRY + 1 real block + EXIT = 3
        assert_eq!(cfg.blocks.len(), 3);
        // ENTRY → real block → EXIT
        assert_eq!(cfg.entry().succs.len(), 1);
        let real_id = cfg.entry().succs[0];
        let real    = cfg.block(real_id);
        assert_eq!(real.succs.len(), 1);
        assert_eq!(real.succs[0], EXIT_BLOCK);
    }

    /// Two-branch if: ifeq → taken / fall-through
    #[test]
    fn test_cfg_conditional_branch() {
        use abyssflower_lib::classfile::instruction::{Instruction, InsnKind};
        use abyssflower_lib::classfile::attribute::{CodeAttribute, ExceptionHandler};

        // Bytecode:
        //  0: ifeq  +5  (jump to offset 5)  3 bytes
        //  3: return               (fall-through)    1 byte
        //  5: return               (branch target)   1 byte
        let code = CodeAttribute {
            max_stack:  1,
            max_locals: 1,
            instructions: vec![
                Instruction { offset: 0, opcode: 0x99 /*ifeq*/, wide: false,
                    kind: InsnKind::Branch { offset: 5 } },   // target = 0 + 5 = 5
                Instruction { offset: 3, opcode: 0xb1 /*return*/, wide: false,
                    kind: InsnKind::NoOperand },
                Instruction { offset: 5, opcode: 0xb1 /*return*/, wide: false,
                    kind: InsnKind::NoOperand },
            ],
            exception_table: vec![],
            attributes:      vec![],
        };

        let cfg = builder::build(&code);
        // ENTRY + 3 real blocks + EXIT = 5
        assert_eq!(cfg.blocks.len(), 5, "expected 5 blocks, got {}\n{}", cfg.blocks.len(), cfg.dump());

        // The ifeq block must have 2 successors
        let ifeq_block = cfg.real_blocks()
            .find(|b| b.start_offset == 0)
            .expect("ifeq block not found");
        assert_eq!(ifeq_block.succs.len(), 2,
            "ifeq block should have 2 successors");
    }
}
