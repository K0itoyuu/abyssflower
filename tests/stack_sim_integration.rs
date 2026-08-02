/// Integration tests for Phase 5: stack simulation + expression lifting.
#[cfg(test)]
mod stack_sim_tests {
    use abyssflower_lib::classfile::constant_pool::ConstantPool;
    use abyssflower_lib::classfile::instruction::{InsnKind, Instruction};
    use abyssflower_lib::ir::expr::{BinOp, Expr};
    use abyssflower_lib::ir::{
        simulate_block, simulate_block_with_context, LocalScope, SimulationContext,
        SimulationErrorKind,
    };
    use abyssflower_lib::types::java_type::JavaType;

    // Minimal stub: build a ConstantPool with no entries for simple tests.
    fn empty_pool_bytes() -> Vec<u8> {
        // constant_pool_count = 1 (just the null slot)
        vec![0x00, 0x01]
    }

    // ── helper: run sim on a raw instruction list ────────────────────────

    fn sim(insns: Vec<Instruction>) -> abyssflower_lib::ir::SimResult {
        // Build a minimal pool
        use abyssflower_lib::classfile::cursor::Cursor;
        let pool_bytes = empty_pool_bytes();
        let mut cur = Cursor::new(&pool_bytes);
        let pool = ConstantPool::parse(&mut cur).unwrap();

        simulate_block(&insns, &pool, vec![], true, "Test", &[])
    }

    fn const_ids(result: &abyssflower_lib::ir::SimResult) -> Vec<String> {
        use abyssflower_lib::ir::ConstValue;
        result
            .stack_out
            .iter()
            .map(|slot| match &slot.expr {
                Expr::Const(value) => match value.value {
                    ConstValue::Int(value) => format!("i{value}"),
                    ConstValue::Long(value) => format!("l{value}"),
                    ConstValue::Double(value) => format!("d{value}"),
                    _ => "other".into(),
                },
                _ => "expr".into(),
            })
            .collect()
    }

    fn stack_insn(offset: u32, opcode: u8) -> Instruction {
        Instruction {
            offset,
            opcode,
            wide: false,
            kind: InsnKind::NoOperand,
        }
    }

    #[test]
    fn local_variable_names_respect_lvt_ranges_and_store_boundaries() {
        use abyssflower_lib::classfile::cursor::Cursor;

        let pool_bytes = empty_pool_bytes();
        let mut cur = Cursor::new(&pool_bytes);
        let pool = ConstantPool::parse(&mut cur).unwrap();
        let scopes = vec![LocalScope {
            slot: 1,
            name: "$i$f$inline".into(),
            start_pc: 2,
            end_pc: 3,
        }];
        let concat = std::collections::HashMap::new();
        let lambdas = std::collections::HashMap::new();
        let context = SimulationContext {
            is_static: true,
            this_class: "Test",
            local_names: &[],
            local_scopes: &scopes,
            local_types: &[],
            return_type: None,
            concat_recipes: &concat,
            lambda_bootstrap: &lambdas,
        };
        let result = simulate_block_with_context(
            &[
                stack_insn(0, 3),  // iconst_0
                stack_insn(1, 60), // istore_1, whose end starts the LVT range
                stack_insn(2, 27), // iload_1 inside the range
                stack_insn(3, 87), // pop after the range
                stack_insn(4, 27), // iload_1 after the range
            ],
            &pool,
            vec![],
            &context,
        );

        let Expr::Assign { lhs, .. } = &result.stmts[0] else {
            panic!("expected local assignment")
        };
        let Expr::LocalVar(stored) = lhs.as_ref() else {
            panic!("expected local assignment target")
        };
        assert_eq!(stored.name.as_deref(), Some("$i$f$inline"));
        let Expr::LocalVar(loaded) = &result.stack_out[0].expr else {
            panic!("expected local load")
        };
        assert_eq!(loaded.name, None);
    }

    // ── constants ────────────────────────────────────────────────────────

    #[test]
    fn test_iconst_push() {
        let result = sim(vec![Instruction {
            offset: 0,
            opcode: 3, /*iconst_0*/
            wide: false,
            kind: InsnKind::NoOperand,
        }]);
        assert_eq!(
            result.stack_out.len(),
            1,
            "iconst_0 should leave one value on stack"
        );
        assert_eq!(result.stack_out[0].ty, JavaType::INT);
    }

    #[test]
    fn test_bipush() {
        let result = sim(vec![Instruction {
            offset: 0,
            opcode: 16, /*bipush*/
            wide: false,
            kind: InsnKind::BytePush { value: 42 },
        }]);
        assert_eq!(result.stack_out.len(), 1);
        match &result.stack_out[0].expr {
            Expr::Const(c) => match c.value {
                abyssflower_lib::ir::ConstValue::Int(v) => assert_eq!(v, 42),
                _ => panic!("expected Int"),
            },
            _ => panic!("expected Const"),
        }
    }

    #[test]
    fn test_sipush() {
        let result = sim(vec![Instruction {
            offset: 0,
            opcode: 17, /*sipush*/
            wide: false,
            kind: InsnKind::ShortPush { value: 1000 },
        }]);
        assert_eq!(result.stack_out.len(), 1);
        if let Expr::Const(c) = &result.stack_out[0].expr {
            if let abyssflower_lib::ir::ConstValue::Int(v) = c.value {
                assert_eq!(v, 1000);
            }
        }
    }

    // ── arithmetic ───────────────────────────────────────────────────────

    #[test]
    fn test_iadd() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3, /*iconst_0*/
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 4, /*iconst_1*/
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 96, /*iadd*/
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(
            result.stack_out.len(),
            1,
            "iadd should consume 2, produce 1"
        );
        assert!(matches!(
            result.stack_out[0].expr,
            Expr::BinOp(BinOp::Add, _, _)
        ));
    }

    #[test]
    fn test_isub() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_1
            Instruction {
                offset: 1,
                opcode: 5,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_2
            Instruction {
                offset: 2,
                opcode: 100,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // isub
        ]);
        assert!(matches!(
            result.stack_out[0].expr,
            Expr::BinOp(BinOp::Sub, _, _)
        ));
    }

    #[test]
    fn test_ineg() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 116,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // ineg
        ]);
        assert!(matches!(
            result.stack_out[0].expr,
            Expr::UnOp(abyssflower_lib::ir::UnOp::Neg, _)
        ));
    }

    // ── stack manipulation ────────────────────────────────────────────────

    #[test]
    fn test_dup() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 89,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // dup
        ]);
        assert_eq!(
            result.stack_out.len(),
            2,
            "dup should produce 2 stack slots from 1"
        );
    }

    #[test]
    fn test_pop() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 87,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // pop
        ]);
        assert_eq!(
            result.stack_out.len(),
            0,
            "pop should consume the top value"
        );
    }

    #[test]
    fn test_swap() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_1
            Instruction {
                offset: 2,
                opcode: 95,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // swap
        ]);
        assert_eq!(result.stack_out.len(), 2);
        // After swap: top is iconst_0 (value 0), bottom is iconst_1 (value 1)
        if let Expr::Const(c) = &result.stack_out[1].expr {
            if let abyssflower_lib::ir::ConstValue::Int(v) = c.value {
                assert_eq!(v, 0);
            }
        }
    }

    #[test]
    fn test_pop2_category2() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 88,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&result), ["i1"]);
    }

    #[test]
    fn test_pop2_category1_pair() {
        let result = sim(vec![stack_insn(0, 4), stack_insn(1, 5), stack_insn(2, 88)]);
        assert!(result.stack_out.is_empty());
    }

    #[test]
    fn test_dup_x1_category1_pair() {
        let result = sim(vec![stack_insn(0, 4), stack_insn(1, 5), stack_insn(2, 90)]);
        assert_eq!(const_ids(&result), ["i2", "i1", "i2"]);
    }

    #[test]
    fn test_dup_x2_category1_triple() {
        let result = sim(vec![
            stack_insn(0, 4),
            stack_insn(1, 5),
            stack_insn(2, 6),
            stack_insn(3, 91),
        ]);
        assert_eq!(const_ids(&result), ["i3", "i1", "i2", "i3"]);
    }

    #[test]
    fn test_dup2_category2() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 92,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&result), ["l0", "l0"]);
    }

    #[test]
    fn test_dup2_category1_pair() {
        let result = sim(vec![stack_insn(0, 4), stack_insn(1, 5), stack_insn(2, 92)]);
        assert_eq!(const_ids(&result), ["i1", "i2", "i1", "i2"]);
    }

    #[test]
    fn test_dup_x2_over_category2() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 91,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&result), ["i1", "l0", "i1"]);
    }

    #[test]
    fn test_dup2_x1_category2() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 93,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&result), ["l0", "i1", "l0"]);
    }

    #[test]
    fn test_dup2_x1_category1_triple() {
        let result = sim(vec![
            stack_insn(0, 4),
            stack_insn(1, 5),
            stack_insn(2, 6),
            stack_insn(3, 93),
        ]);
        assert_eq!(const_ids(&result), ["i2", "i3", "i1", "i2", "i3"]);
    }

    #[test]
    fn test_dup2_x2_all_category_forms() {
        let form1 = sim(vec![
            Instruction {
                offset: 0,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 5,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 6,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 3,
                opcode: 7,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 4,
                opcode: 94,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&form1), ["i3", "i4", "i1", "i2", "i3", "i4"]);

        let form2 = sim(vec![
            Instruction {
                offset: 0,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 5,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 3,
                opcode: 94,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&form2), ["i1", "i2", "l0", "i1", "i2"]);

        let form3 = sim(vec![
            Instruction {
                offset: 0,
                opcode: 4,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 5,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 3,
                opcode: 94,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&form3), ["l0", "i1", "i2", "l0"]);

        let form4 = sim(vec![
            Instruction {
                offset: 0,
                opcode: 9,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 1,
                opcode: 14,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            Instruction {
                offset: 2,
                opcode: 94,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        assert_eq!(const_ids(&form4), ["d0", "l0", "d0"]);
    }

    #[test]
    fn test_stack_operations_report_underflow() {
        for (opcode, setup) in [
            (87, vec![]),
            (88, vec![4]),
            (89, vec![]),
            (90, vec![4]),
            (91, vec![4, 5]),
            (92, vec![4]),
            (93, vec![4, 5]),
            (94, vec![]),
            (94, vec![4, 9]),
            (94, vec![4, 5, 6]),
            (95, vec![4]),
        ] {
            let mut instructions: Vec<_> = setup
                .into_iter()
                .enumerate()
                .map(|(offset, setup_opcode)| stack_insn(offset as u32, setup_opcode))
                .collect();
            let offset = instructions.len() as u32;
            instructions.push(stack_insn(offset, opcode));
            let result = sim(instructions);
            assert_eq!(result.errors.len(), 1, "opcode {opcode}");
            assert_eq!(result.errors[0].kind, SimulationErrorKind::StackUnderflow);
            assert_eq!(result.errors[0].offset, offset);
            assert_eq!(result.errors[0].opcode, opcode);
        }
    }

    #[test]
    fn test_stack_operations_report_invalid_category_forms_without_mutation() {
        let cases = [
            (87, vec![9]),
            (88, vec![9, 4]),
            (89, vec![9]),
            (90, vec![9, 4]),
            (91, vec![9, 4, 5]),
            (92, vec![9, 4]),
            (93, vec![9, 9]),
            (94, vec![9, 4, 9]),
            (95, vec![9, 4]),
        ];
        for (opcode, setup) in cases {
            let mut instructions: Vec<_> = setup
                .iter()
                .enumerate()
                .map(|(offset, setup_opcode)| stack_insn(offset as u32, *setup_opcode))
                .collect();
            let before = sim(instructions.clone());
            let offset = instructions.len() as u32;
            instructions.push(stack_insn(offset, opcode));
            let result = sim(instructions);
            assert_eq!(result.errors.len(), 1, "opcode {opcode}");
            assert_eq!(
                result.errors[0].kind,
                SimulationErrorKind::InvalidStackForm,
                "opcode {opcode}"
            );
            assert_eq!(const_ids(&result), const_ids(&before), "opcode {opcode}");
        }
    }

    // ── casts ─────────────────────────────────────────────────────────────

    #[test]
    fn test_i2l_cast() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 133,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // i2l
        ]);
        assert_eq!(result.stack_out.len(), 1);
        assert!(matches!(
            result.stack_out[0].expr,
            Expr::Cast(abyssflower_lib::ir::CastKind::I2L, _, _)
        ));
        assert_eq!(result.stack_out[0].ty, JavaType::LONG);
    }

    // ── locals ────────────────────────────────────────────────────────────

    #[test]
    fn test_istore_iload() {
        let result = sim(vec![
            // bipush 99
            Instruction {
                offset: 0,
                opcode: 16,
                wide: false,
                kind: InsnKind::BytePush { value: 99 },
            },
            // istore_1 (opcode 60)
            Instruction {
                offset: 2,
                opcode: 60,
                wide: false,
                kind: InsnKind::NoOperand,
            },
            // iload_1 (opcode 27)
            Instruction {
                offset: 3,
                opcode: 27,
                wide: false,
                kind: InsnKind::NoOperand,
            },
        ]);
        // istore emits an Assign statement
        assert!(!result.stmts.is_empty(), "istore should emit an assignment");
        // iload puts the var back on the stack
        assert_eq!(result.stack_out.len(), 1);
        assert!(matches!(&result.stack_out[0].expr, Expr::LocalVar(lv) if lv.slot == 1));
    }

    // ── return ────────────────────────────────────────────────────────────

    #[test]
    fn test_return_void() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 177,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // return
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(result.stmts[0], Expr::Return(None)));
    }

    #[test]
    fn test_ireturn() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 3,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // iconst_0
            Instruction {
                offset: 1,
                opcode: 172,
                wide: false,
                kind: InsnKind::NoOperand,
            }, // ireturn
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(&result.stmts[0], Expr::Return(Some(_))));
    }

    // ── smoke test: simulate all methods in a real class ──────────────────

    #[test]
    fn test_simulate_repository_class() {
        use abyssflower_lib::ClassFile;

        let bytes = include_bytes!("java_classes/fixture/ControlFlowFixture.class");
        let cf = ClassFile::parse(bytes).unwrap();

        for method in &cf.methods {
            let code = match method.code() {
                Some(c) => c,
                None => continue,
            };
            let is_static = method.is_static();

            let result = simulate_block(
                &code.instructions,
                &cf.constant_pool,
                vec![],
                is_static,
                &cf.this_class,
                &[],
            );
            // Should not panic and should produce some output for non-empty methods
            if !code.instructions.is_empty() {
                assert!(
                    result.stmts.len() + result.stack_out.len() > 0 || code.instructions.len() == 1,
                    "Expected some output from {}.{}",
                    cf.this_class,
                    method.name
                );
            }
        }
    }

    // ── iinc ─────────────────────────────────────────────────────────────

    #[test]
    fn test_iinc() {
        let result = sim(vec![
            Instruction {
                offset: 0,
                opcode: 132,
                wide: false,
                kind: InsnKind::Iinc {
                    index: 1,
                    const_: 1,
                },
            }, // iinc 1, 1
        ]);
        assert_eq!(result.stmts.len(), 1);
        assert!(matches!(
            result.stmts[0],
            Expr::IInc {
                slot: 1,
                delta: 1,
                ..
            }
        ));
    }
}
