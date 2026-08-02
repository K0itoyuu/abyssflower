//! Explicit method-level dependencies shared by source renderers.

use crate::cfg::{BlockId, Cfg, ENTRY_BLOCK, EXIT_BLOCK};
use crate::classfile::attribute::Attribute;
use crate::classfile::attribute::CodeAttribute;
use crate::classfile::constant_pool::{ConstantPool, CpEntry};
use crate::classfile::instruction::Instruction;
use crate::classfile::ClassFile;
use crate::codegen::stmt_writer::{lvt_entries, LvtEntry};
use crate::ir::stack_sim::{
    simulate_block_with_context, LocalScope, SimResult, SimulationContext, SlotInfo,
};
use crate::ir::LambdaBootstrap;
use crate::ir::{BinOp, ConstExpr, ConstValue, Expr, LocalVarExpr, TernaryCondition, UnOp};
use crate::types::descriptor::{parse_field_descriptor, MethodDescriptor};
use crate::types::java_type::JavaType;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;

pub struct RenderContext<'a> {
    pub code: &'a CodeAttribute,
    pub pool: &'a ConstantPool,
    pub class: &'a ClassFile,
    pub is_static: bool,
    pub this_class: &'a str,
    pub names: Vec<(u16, String)>,
    pub local_scopes: Vec<LocalScope>,
    pub lvt: Vec<LvtEntry>,
    pub local_types: Vec<(u16, JavaType, Option<String>)>,
    pub return_type: Option<JavaType>,
    pub concat_recipes: HashMap<u16, String>,
    pub lambda_bootstrap: HashMap<u16, LambdaBootstrap>,
    pub is_coroutine_state_machine: bool,
    pub block_entries: HashMap<BlockId, BlockEntryState>,
    pub block_instructions: HashMap<BlockId, Vec<Instruction>>,
    pub block_successors: HashMap<BlockId, Vec<BlockId>>,
    pub value_producing_loops: Vec<ValueProducingLoop>,
    pub hoisted_locals: Vec<(u16, JavaType, String)>,
    pub completed_value_producing_loops: RefCell<Vec<ValueProducingLoop>>,
    pub declared_slots: RefCell<HashSet<u16>>,
    pub declared_local_names: RefCell<HashMap<u16, String>>,
    pub local_assignment_counts: HashMap<u16, usize>,
}

#[derive(Debug, Clone)]
pub struct ValueProducingLoop {
    pub header_block: BlockId,
    pub body_blocks: Vec<BlockId>,
    pub predicate_block: BlockId,
    pub predicate_negated: bool,
    pub iterator_slot: u16,
    pub element_slot: u16,
    pub result_type: JavaType,
    pub success_value: Expr,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct BlockEntryState {
    pub stack: Vec<SlotInfo>,
    pub local_types: Vec<(u16, JavaType, Option<String>)>,
    pub local_values: HashMap<u16, Expr>,
}

impl<'a> RenderContext<'a> {
    pub fn new(code: &'a CodeAttribute, class: &'a ClassFile, is_static: bool) -> Self {
        let mut lvt = lvt_entries(code);
        disambiguate_lvt_names(&mut lvt);
        let names = lvt
            .iter()
            .map(|entry| (entry.slot, entry.name.clone()))
            .collect();
        let local_scopes = lvt
            .iter()
            .map(|entry| LocalScope {
                slot: entry.slot,
                name: entry.name.clone(),
                start_pc: u32::from(entry.start_pc),
                end_pc: u32::from(entry.start_pc) + u32::from(entry.length),
            })
            .collect();
        let local_types = lvt
            .iter()
            .filter_map(|entry| {
                parse_field_descriptor(&entry.descriptor)
                    .ok()
                    .map(|(ty, _)| (entry.slot, ty, Some(entry.name.clone())))
            })
            .collect();
        Self {
            code,
            pool: &class.constant_pool,
            class,
            is_static,
            this_class: &class.this_class,
            names,
            local_scopes,
            lvt,
            local_types,
            return_type: None,
            concat_recipes: concat_recipes(class),
            lambda_bootstrap: HashMap::new(),
            is_coroutine_state_machine: detects_coroutine_state_machine(code, class),
            block_entries: HashMap::new(),
            block_instructions: HashMap::new(),
            block_successors: HashMap::new(),
            value_producing_loops: Vec::new(),
            hoisted_locals: Vec::new(),
            completed_value_producing_loops: RefCell::new(Vec::new()),
            declared_slots: RefCell::new(if is_static {
                HashSet::new()
            } else {
                HashSet::from([0])
            }),
            declared_local_names: RefCell::new(if is_static {
                HashMap::new()
            } else {
                HashMap::from([(0, "this".into())])
            }),
            local_assignment_counts: count_local_assignments(code),
        }
    }

    pub fn for_method(
        code: &'a CodeAttribute,
        class: &'a ClassFile,
        is_static: bool,
        descriptor: &str,
        parameter_names: &[String],
    ) -> Self {
        let mut context = Self::new(code, class, is_static);
        if let Ok(method) = MethodDescriptor::parse(descriptor) {
            let mut slot = if is_static { 0 } else { 1 };
            for (index, ty) in method.params.into_iter().enumerate() {
                context.declared_slots.borrow_mut().insert(slot);
                let source_name = parameter_names
                    .get(index)
                    .filter(|name| !name.is_empty())
                    .cloned();
                if let Some(name) = source_name.as_ref() {
                    context.bind_slot_name(slot, name);
                }
                let declared_name = source_name.clone().or_else(|| {
                    context
                        .local_scopes
                        .iter()
                        .find(|scope| scope.slot == slot && scope.start_pc == 0)
                        .map(|scope| scope.name.clone())
                });
                if let Some(name) = declared_name {
                    context.declared_local_names.borrow_mut().insert(slot, name);
                }
                if let Some(existing) = context
                    .local_types
                    .iter_mut()
                    .find(|(local, _, _)| *local == slot)
                {
                    // The descriptor is authoritative for parameter types and
                    // Kotlin metadata is authoritative for source names.
                    existing.1 = ty.clone();
                    if source_name.is_some() {
                        existing.2 = source_name;
                    }
                } else {
                    context.local_types.push((slot, ty.clone(), source_name));
                }
                slot += if matches!(ty, JavaType::LONG | JavaType::DOUBLE) {
                    2
                } else {
                    1
                };
            }
            context.return_type = Some(method.return_type);
        }
        context
    }

    /// Give the implicit JVM dispatch receiver a Kotlin qualified-this
    /// spelling. In member extensions, plain this is the extension receiver.
    pub fn with_dispatch_receiver_label(mut self, label: &str) -> Self {
        if !self.is_static {
            self.bind_slot_name(0, &format!("$kotlin$dispatch$this@{label}"));
        }
        self
    }

    fn bind_slot_name(&mut self, slot: u16, name: &str) {
        if self.declared_slots.borrow().contains(&slot) {
            self.declared_local_names
                .borrow_mut()
                .insert(slot, name.to_owned());
        }
        let mut found_name = false;
        for (candidate_slot, candidate_name) in &mut self.names {
            if *candidate_slot == slot {
                *candidate_name = name.to_owned();
                found_name = true;
            }
        }
        if !found_name {
            self.names.push((slot, name.to_owned()));
        }

        let mut found_scope = false;
        for scope in &mut self.local_scopes {
            // Do not rename a later local reusing the same JVM slot.
            if scope.slot == slot && scope.start_pc == 0 {
                scope.name = name.to_owned();
                found_scope = true;
            }
        }
        if !found_scope {
            self.local_scopes.push(LocalScope {
                slot,
                name: name.to_owned(),
                start_pc: 0,
                end_pc: u32::MAX,
            });
        }

        if let Some((_, _, candidate_name)) = self
            .local_types
            .iter_mut()
            .find(|(candidate_slot, _, _)| *candidate_slot == slot)
        {
            *candidate_name = Some(name.to_owned());
        }
    }

    pub fn with_lambda_bootstrap(
        mut self,
        lambda_bootstrap: &HashMap<u16, LambdaBootstrap>,
    ) -> Self {
        self.lambda_bootstrap.clone_from(lambda_bootstrap);
        self
    }

    pub fn with_coroutine_state_machine(mut self, enabled: bool) -> Self {
        self.is_coroutine_state_machine = enabled;
        self
    }

    /// Compute verifier-style entry states for every basic block. This is
    /// deliberately expression-aware: identical predecessor values survive a
    /// merge, while conflicting values become an explicit phi placeholder.
    pub fn with_cfg_dataflow(mut self, cfg: &Cfg) -> Self {
        self.block_entries = self.compute_block_entries(cfg);
        self.block_instructions = cfg
            .real_blocks()
            .map(|block| (block.id, block.instructions.clone()))
            .collect();
        self.block_successors = cfg
            .real_blocks()
            .map(|block| (block.id, block.succs.clone()))
            .collect();
        self.value_producing_loops = self.find_value_producing_iterator_loops(cfg);
        self.hoisted_locals = self.find_non_dominating_locals(cfg);
        self.hoisted_locals.retain(|(slot, _, _)| {
            !self.value_producing_loops.iter().any(|value_loop| {
                let body = value_loop
                    .body_blocks
                    .iter()
                    .copied()
                    .collect::<HashSet<_>>();
                let defined_in_body = value_loop.body_blocks.iter().any(|block| {
                    cfg.block(*block)
                        .instructions
                        .iter()
                        .any(|instruction| instruction_local_store_slot(instruction) == Some(*slot))
                });
                let loaded_outside = cfg.real_blocks().any(|block| {
                    !body.contains(&block.id)
                        && block.instructions.iter().any(|instruction| {
                            instruction_local_load_slot(instruction) == Some(*slot)
                        })
                });
                *slot == value_loop.iterator_slot
                    || *slot == value_loop.element_slot
                    || (defined_in_body && !loaded_outside)
            })
        });
        self
    }

    fn find_value_producing_iterator_loops(&self, cfg: &Cfg) -> Vec<ValueProducingLoop> {
        let dom = crate::cfg::DomTree::compute(cfg);
        let mut result = Vec::new();
        for natural_loop in crate::cfg::find_natural_loops(cfg, &dom) {
            let header = cfg.block(natural_loop.header);
            let iterator_slot =
                header
                    .instructions
                    .iter()
                    .enumerate()
                    .find_map(|(index, instruction)| {
                        invocation_name(instruction, self.pool)
                            .is_some_and(|name| name == "hasNext")
                            .then(|| {
                                header.instructions[..index]
                                    .iter()
                                    .rev()
                                    .find_map(instruction_local_load_slot)
                            })?
                    });
            let Some(iterator_slot) = iterator_slot else {
                continue;
            };

            let mut ordered = natural_loop
                .body
                .iter()
                .flat_map(|block| cfg.block(*block).instructions.iter())
                .collect::<Vec<_>>();
            ordered.sort_by_key(|instruction| instruction.offset);
            let element_slot = ordered.iter().enumerate().find_map(|(index, instruction)| {
                invocation_name(instruction, self.pool)
                    .is_some_and(|name| name == "next")
                    .then(|| {
                        ordered[index + 1..]
                            .iter()
                            .find_map(|candidate| instruction_local_store_slot(candidate))
                    })?
            });
            let Some(element_slot) = element_slot else {
                continue;
            };

            let body = natural_loop.body.iter().copied().collect::<HashSet<_>>();
            let exits = natural_loop
                .body
                .iter()
                .flat_map(|block| {
                    cfg.block(*block)
                        .succs
                        .iter()
                        .filter(|successor| !body.contains(successor) && **successor != EXIT_BLOCK)
                        .map(|successor| (*block, *successor))
                })
                .collect::<Vec<_>>();
            if exits.len() != 2 || exits[0].1 == exits[1].1 {
                continue;
            }

            let classify_exit = |target: BlockId| {
                let block = cfg.block(target);
                let state = self.block_entry(target);
                let simulation = self.simulate_state(&block.instructions, &state);
                let value = simulation.stack_out.last()?.expr.clone();
                let merge = (block.succs.len() == 1).then_some(block.succs[0])?;
                Some((value, merge))
            };
            let Some((first_value, first_merge)) = classify_exit(exits[0].1) else {
                continue;
            };
            let Some((second_value, second_merge)) = classify_exit(exits[1].1) else {
                continue;
            };
            if first_merge != second_merge {
                continue;
            }
            let (success_exit, success_value) =
                if expression_is_local_slot(&first_value, element_slot)
                    && matches!(second_value, Expr::Null)
                {
                    (exits[0], first_value)
                } else if expression_is_local_slot(&second_value, element_slot)
                    && matches!(first_value, Expr::Null)
                {
                    (exits[1], second_value)
                } else {
                    continue;
                };
            let predicate = cfg.block(success_exit.0);
            if predicate.succs.len() != 2
                || !predicate
                    .last_insn()
                    .is_some_and(|instruction| is_conditional_branch(instruction.opcode))
            {
                continue;
            }
            let predicate_negated = if predicate.succs[0] == success_exit.1 {
                false
            } else if predicate.succs[1] == success_exit.1 {
                true
            } else {
                continue;
            };
            let result_type = cfg
                .block(first_merge)
                .instructions
                .iter()
                .find_map(|instruction| {
                    if instruction.opcode != crate::classfile::opcodes::opc::checkcast {
                        return None;
                    }
                    let crate::classfile::instruction::InsnKind::Cp { index } = instruction.kind
                    else {
                        return None;
                    };
                    match self.pool.get(index).ok()? {
                        CpEntry::Class(name) => Some(JavaType::object(name)),
                        _ => None,
                    }
                })
                .or_else(|| {
                    self.block_entry(success_exit.1)
                        .stack
                        .last()
                        .map(|slot| slot.ty.clone())
                        .filter(|ty| *ty != JavaType::UNKNOWN)
                })
                .unwrap_or(JavaType::object("java/lang/Object"));
            let success_value = if expression_has_type(&success_value, &result_type) {
                success_value
            } else {
                Expr::Cast(
                    crate::ir::CastKind::CheckCast,
                    result_type.clone(),
                    Box::new(success_value),
                )
            };
            result.push(ValueProducingLoop {
                header_block: natural_loop.header,
                body_blocks: natural_loop.body.clone(),
                predicate_block: success_exit.0,
                predicate_negated,
                iterator_slot,
                element_slot,
                result_type,
                success_value,
                name: format!("__abyss_loop_result_{}", natural_loop.header),
            });
        }
        result
    }

    fn find_non_dominating_locals(&self, cfg: &Cfg) -> Vec<(u16, JavaType, String)> {
        let dom = crate::cfg::DomTree::compute(cfg);
        let natural_loops = crate::cfg::find_natural_loops(cfg, &dom);
        let mut stores = HashMap::<u16, Vec<(BlockId, usize)>>::new();
        let mut loads = HashMap::<u16, Vec<(BlockId, usize)>>::new();
        for block in cfg.real_blocks() {
            for (index, instruction) in block.instructions.iter().enumerate() {
                if let Some(slot) = instruction_local_store_slot(instruction) {
                    stores.entry(slot).or_default().push((block.id, index));
                }
                if let Some(slot) = instruction_local_load_slot(instruction) {
                    loads.entry(slot).or_default().push((block.id, index));
                }
            }
        }
        // Some source values cross block boundaries only on the operand
        // stack. By the time recovery renders them, their LocalVar leaves are
        // embedded in a block-entry expression and no longer correspond to a
        // load instruction in that block. Count those leaves as entry uses so
        // the declaration analysis does not incorrectly confine them to a
        // reconstructed loop body.
        for (&block, state) in &self.block_entries {
            let mut referenced = HashSet::new();
            for value in state
                .stack
                .iter()
                .map(|slot| &slot.expr)
                .chain(state.local_values.values())
            {
                collect_local_slots(value, &mut referenced);
            }
            for slot in referenced {
                loads.entry(slot).or_default().push((block, 0));
            }
        }

        let declared = self.declared_slots.borrow();
        let mut result = Vec::new();
        for (slot, uses) in loads {
            if declared.contains(&slot) {
                continue;
            }
            let Some(definitions) = stores.get(&slot) else {
                continue;
            };
            let has_dominating_definition =
                definitions.iter().any(|(definition, definition_index)| {
                    uses.iter().all(|(use_block, use_index)| {
                        if definition == use_block {
                            definition_index < use_index
                        } else {
                            dom.dominates(*definition, *use_block)
                        }
                    })
                });
            // JVM bytecode may load a loop element only on the loop's break edge,
            // so the element store dominates that load in the CFG.  Kotlin still
            // cannot reference a variable declared lexically inside the loop after
            // the loop.  Hoist slots whose definitions are all inside one natural
            // loop and which have at least one use outside it.
            let crosses_loop_scope = natural_loops.iter().any(|natural_loop| {
                let body = natural_loop.body.iter().copied().collect::<HashSet<_>>();
                definitions
                    .iter()
                    .all(|(definition, _)| body.contains(definition))
                    && uses.iter().any(|(use_block, _)| !body.contains(use_block))
            });
            if has_dominating_definition && !crosses_loop_scope {
                continue;
            }
            let Some(ty) = self
                .block_entries
                .values()
                .flat_map(|state| state.local_types.iter())
                .find(|(candidate, ty, _)| *candidate == slot && *ty != JavaType::UNKNOWN)
                .map(|(_, ty, _)| ty.clone())
                .or_else(|| {
                    self.local_types
                        .iter()
                        .find(|(candidate, ty, _)| *candidate == slot && *ty != JavaType::UNKNOWN)
                        .map(|(_, ty, _)| ty.clone())
                })
            else {
                continue;
            };
            let name = self
                .local_scopes
                .iter()
                .filter(|scope| scope.slot == slot)
                .min_by_key(|scope| scope.start_pc)
                .map(|scope| scope.name.clone())
                .unwrap_or_else(|| format!("var{slot}"));
            result.push((slot, ty, name));
        }
        result.sort_by_key(|(slot, _, _)| *slot);
        result
    }

    pub fn block_entry(&self, block: BlockId) -> BlockEntryState {
        self.block_entries
            .get(&block)
            .cloned()
            .unwrap_or_else(|| BlockEntryState {
                stack: Vec::new(),
                local_types: self.local_types.clone(),
                local_values: HashMap::new(),
            })
    }

    pub fn simulation(&self) -> SimulationContext<'_> {
        SimulationContext {
            is_static: self.is_static,
            this_class: self.this_class,
            local_names: &self.names,
            local_scopes: &self.local_scopes,
            local_types: &self.local_types,
            return_type: self.return_type.as_ref(),
            concat_recipes: &self.concat_recipes,
            lambda_bootstrap: &self.lambda_bootstrap,
        }
    }

    pub fn simulate(
        &self,
        instructions: &[Instruction],
        initial_stack: Vec<SlotInfo>,
    ) -> SimResult {
        let simulation = self.simulation();
        simulate_block_with_context(instructions, self.pool, initial_stack, &simulation)
    }

    pub fn simulate_state(
        &self,
        instructions: &[Instruction],
        state: &BlockEntryState,
    ) -> SimResult {
        let simulation = SimulationContext {
            is_static: self.is_static,
            this_class: self.this_class,
            local_names: &self.names,
            local_scopes: &self.local_scopes,
            local_types: &state.local_types,
            return_type: self.return_type.as_ref(),
            concat_recipes: &self.concat_recipes,
            lambda_bootstrap: &self.lambda_bootstrap,
        };
        simulate_block_with_context(instructions, self.pool, state.stack.clone(), &simulation)
    }

    fn compute_block_entries(&self, cfg: &Cfg) -> HashMap<BlockId, BlockEntryState> {
        use crate::classfile::opcodes::opc;
        use std::collections::VecDeque;

        let mut entries = HashMap::new();
        let mut incoming_states = HashMap::<(BlockId, BlockId), BlockEntryState>::new();
        let mut queue = VecDeque::new();
        for &first in &cfg.entry().succs {
            entries.insert(
                first,
                BlockEntryState {
                    stack: Vec::new(),
                    local_types: self.local_types.clone(),
                    local_values: HashMap::new(),
                },
            );
            incoming_states.insert((first, ENTRY_BLOCK), entries[&first].clone());
            queue.push_back(first);
        }

        let mut iterations = 0usize;
        let limit = cfg.len().saturating_mul(32).max(32);
        while let Some(block_id) = queue.pop_front() {
            if block_id == ENTRY_BLOCK || block_id == EXIT_BLOCK || iterations >= limit {
                continue;
            }
            iterations += 1;
            let block = cfg.block(block_id);
            let entry = entries.get(&block_id).cloned().unwrap_or_default();
            let result = self.simulate_state(&block.instructions, &entry);
            let mut normal_stack = result.stack_out;
            if let Some(last) = block.last_insn() {
                let pops = match last.opcode {
                    opc::if_icmpeq
                    | opc::if_icmpne
                    | opc::if_icmplt
                    | opc::if_icmpge
                    | opc::if_icmpgt
                    | opc::if_icmple
                    | opc::if_acmpeq
                    | opc::if_acmpne => 2,
                    opc::ifeq
                    | opc::ifne
                    | opc::iflt
                    | opc::ifge
                    | opc::ifgt
                    | opc::ifle
                    | opc::ifnull
                    | opc::ifnonnull
                    | opc::tableswitch
                    | opc::lookupswitch => 1,
                    _ => 0,
                };
                for _ in 0..pops {
                    normal_stack.pop();
                }
            }
            let mut exit_locals = entry.local_types.clone();
            let mut exit_values = entry.local_values.clone();
            for (slot, value, ty) in result.locals {
                exit_values.insert(slot, value);
                if let Some(existing) = exit_locals.iter_mut().find(|(s, _, _)| *s == slot) {
                    existing.1 = ty;
                } else {
                    let name = self
                        .names
                        .iter()
                        .find(|(s, _)| *s == slot)
                        .map(|(_, n)| n.clone());
                    exit_locals.push((slot, ty, name));
                }
            }
            let normal = BlockEntryState {
                stack: normal_stack,
                local_types: exit_locals.clone(),
                local_values: exit_values.clone(),
            };
            for &succ in &block.succs {
                if succ != EXIT_BLOCK
                    && merge_entry(
                        &mut entries,
                        &mut incoming_states,
                        block_id,
                        succ,
                        &normal,
                        cfg,
                        self,
                    )
                {
                    queue.push_back(succ);
                }
            }
            for &handler in &block.succ_exceptions {
                let catch_type = cfg
                    .exception_ranges
                    .iter()
                    .find(|range| range.handler == handler)
                    .and_then(|range| range.catch_type.as_deref())
                    .unwrap_or("java/lang/Throwable");
                let ty = JavaType::object(catch_type);
                let exceptional = BlockEntryState {
                    stack: vec![SlotInfo {
                        expr: Expr::LocalVar(LocalVarExpr {
                            slot: u16::MAX,
                            ty: ty.clone(),
                            name: Some("exception".into()),
                        }),
                        ty,
                    }],
                    local_types: exit_locals.clone(),
                    local_values: exit_values.clone(),
                };
                if merge_entry(
                    &mut entries,
                    &mut incoming_states,
                    block_id,
                    handler,
                    &exceptional,
                    cfg,
                    self,
                ) {
                    queue.push_back(handler);
                }
            }
        }
        entries
    }
}

/// Kotlin declarations share a source namespace even when the JVM locals use
/// different slots and non-overlapping LocalVariableTable ranges. Kotlin's
/// inline lowering commonly emits several locals named `this_$iv` or `it$iv`
/// in one method. Keep the earliest identity's debug name and give later,
/// distinct identities a stable suffix before any IR expressions are built.
///
/// Repeated table ranges for the same `(slot, name, descriptor)` remain one
/// identity; compilers may split a single source local's debug range.
fn disambiguate_lvt_names(entries: &mut [LvtEntry]) {
    let original_names = entries
        .iter()
        .map(|entry| entry.name.clone())
        .collect::<HashSet<_>>();
    let mut occupied = original_names;
    let mut identity_names = HashMap::<(u16, String, String), String>::new();
    let mut first_identity = HashMap::<String, (u16, String, String)>::new();
    let mut order = (0..entries.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let entry = &entries[*index];
        (entry.start_pc, entry.slot, entry.length)
    });

    for index in order {
        let raw_name = entries[index].name.clone();
        let identity = (
            entries[index].slot,
            raw_name.clone(),
            entries[index].descriptor.clone(),
        );
        if let Some(name) = identity_names.get(&identity) {
            entries[index].name.clone_from(name);
            continue;
        }

        let name = if first_identity.contains_key(&raw_name) {
            let base = format!("{raw_name}_{}", entries[index].slot);
            let mut candidate = base.clone();
            let mut suffix = 2usize;
            while occupied.contains(&candidate) {
                candidate = format!("{base}_{suffix}");
                suffix += 1;
            }
            candidate
        } else {
            first_identity.insert(raw_name.clone(), identity.clone());
            raw_name
        };
        occupied.insert(name.clone());
        identity_names.insert(identity, name.clone());
        entries[index].name = name;
    }
}

fn collect_local_slots(expr: &Expr, slots: &mut HashSet<u16>) {
    match expr {
        Expr::LocalVar(local) => {
            slots.insert(local.slot);
        }
        Expr::BinOp(_, left, right)
        | Expr::Assign {
            lhs: left,
            rhs: right,
        } => {
            collect_local_slots(left, slots);
            collect_local_slots(right, slots);
        }
        Expr::UnOp(_, value)
        | Expr::Cast(_, _, value)
        | Expr::InstanceOf(value, _)
        | Expr::ArrayLength(value)
        | Expr::Monitor { object: value, .. }
        | Expr::Throw(value) => collect_local_slots(value, slots),
        Expr::Ternary {
            cond,
            then_expr,
            else_expr,
        } => {
            if let TernaryCondition::Expression(condition) = cond {
                collect_local_slots(condition, slots);
            }
            collect_local_slots(then_expr, slots);
            collect_local_slots(else_expr, slots);
        }
        Expr::SwitchExpression { selector, arms } => {
            collect_local_slots(selector, slots);
            for (_, value) in arms {
                collect_local_slots(value, slots);
            }
        }
        Expr::Field { object, value, .. } => {
            if let Some(object) = object {
                collect_local_slots(object, slots);
            }
            if let Some(value) = value {
                collect_local_slots(value, slots);
            }
        }
        Expr::Invoke { object, args, .. } => {
            if let Some(object) = object {
                collect_local_slots(object, slots);
            }
            for argument in args {
                collect_local_slots(argument, slots);
            }
        }
        Expr::InvokeDynamic { args, .. } | Expr::New { args, .. } => {
            for argument in args {
                collect_local_slots(argument, slots);
            }
        }
        Expr::ArrayLoad { array, index, .. } => {
            collect_local_slots(array, slots);
            collect_local_slots(index, slots);
        }
        Expr::ArrayStore {
            array,
            index,
            value,
        } => {
            collect_local_slots(array, slots);
            collect_local_slots(index, slots);
            collect_local_slots(value, slots);
        }
        Expr::NewArray {
            dimensions,
            initializer,
            ..
        } => {
            for dimension in dimensions {
                collect_local_slots(dimension, slots);
            }
            if let Some(initializer) = initializer {
                for value in initializer {
                    collect_local_slots(value, slots);
                }
            }
        }
        Expr::Return(Some(value)) => collect_local_slots(value, slots),
        Expr::Const(_)
        | Expr::Null
        | Expr::This(_)
        | Expr::IInc { .. }
        | Expr::Return(None)
        | Expr::Opaque { .. } => {}
    }
}

fn count_local_assignments(code: &CodeAttribute) -> HashMap<u16, usize> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    let mut counts = HashMap::new();
    for instruction in &code.instructions {
        let slot = match instruction.kind {
            InsnKind::Iinc { index, .. } => Some(index),
            InsnKind::LocalVar { index }
                if matches!(
                    instruction.opcode,
                    opc::istore | opc::lstore | opc::fstore | opc::dstore | opc::astore
                ) =>
            {
                Some(index)
            }
            _ => match instruction.opcode {
                opc::istore_0 | opc::lstore_0 | opc::fstore_0 | opc::dstore_0 | opc::astore_0 => {
                    Some(0)
                }
                opc::istore_1 | opc::lstore_1 | opc::fstore_1 | opc::dstore_1 | opc::astore_1 => {
                    Some(1)
                }
                opc::istore_2 | opc::lstore_2 | opc::fstore_2 | opc::dstore_2 | opc::astore_2 => {
                    Some(2)
                }
                opc::istore_3 | opc::lstore_3 | opc::fstore_3 | opc::dstore_3 | opc::astore_3 => {
                    Some(3)
                }
                _ => None,
            },
        };
        if let Some(slot) = slot {
            *counts.entry(slot).or_insert(0) += 1;
        }
    }
    counts
}

fn instruction_local_store_slot(instruction: &Instruction) -> Option<u16> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    match instruction.kind {
        InsnKind::LocalVar { index }
            if matches!(
                instruction.opcode,
                opc::istore | opc::lstore | opc::fstore | opc::dstore | opc::astore
            ) =>
        {
            Some(index)
        }
        _ => match instruction.opcode {
            opc::istore_0 | opc::lstore_0 | opc::fstore_0 | opc::dstore_0 | opc::astore_0 => {
                Some(0)
            }
            opc::istore_1 | opc::lstore_1 | opc::fstore_1 | opc::dstore_1 | opc::astore_1 => {
                Some(1)
            }
            opc::istore_2 | opc::lstore_2 | opc::fstore_2 | opc::dstore_2 | opc::astore_2 => {
                Some(2)
            }
            opc::istore_3 | opc::lstore_3 | opc::fstore_3 | opc::dstore_3 | opc::astore_3 => {
                Some(3)
            }
            _ => None,
        },
    }
}

fn instruction_local_load_slot(instruction: &Instruction) -> Option<u16> {
    use crate::classfile::instruction::InsnKind;
    use crate::classfile::opcodes::opc;

    match instruction.kind {
        InsnKind::LocalVar { index }
            if matches!(
                instruction.opcode,
                opc::iload | opc::lload | opc::fload | opc::dload | opc::aload
            ) =>
        {
            Some(index)
        }
        _ => match instruction.opcode {
            opc::iload_0 | opc::lload_0 | opc::fload_0 | opc::dload_0 | opc::aload_0 => Some(0),
            opc::iload_1 | opc::lload_1 | opc::fload_1 | opc::dload_1 | opc::aload_1 => Some(1),
            opc::iload_2 | opc::lload_2 | opc::fload_2 | opc::dload_2 | opc::aload_2 => Some(2),
            opc::iload_3 | opc::lload_3 | opc::fload_3 | opc::dload_3 | opc::aload_3 => Some(3),
            _ => None,
        },
    }
}

fn merge_entry(
    entries: &mut HashMap<BlockId, BlockEntryState>,
    incoming_states: &mut HashMap<(BlockId, BlockId), BlockEntryState>,
    source: BlockId,
    block: BlockId,
    incoming: &BlockEntryState,
    cfg: &Cfg,
    context: &RenderContext<'_>,
) -> bool {
    let before = entries.get(&block).map(state_fingerprint);
    incoming_states.insert((block, source), incoming.clone());
    let mut contributions = incoming_states
        .iter()
        .filter(|((target, _), _)| *target == block)
        .map(|((_, source), state)| (*source, state.clone()))
        .collect::<Vec<_>>();
    contributions.sort_by_key(|(source, _)| *source);
    let Some((first_source, mut current)) = contributions.first().cloned() else {
        return false;
    };
    let switch_values = recover_switch_entry_values(entries, cfg, context, block, &contributions);
    let mut previous_sources = vec![first_source];
    let offset = cfg.block(block).start_offset;
    for (right_source, right_state) in contributions.iter().skip(1) {
        if current.stack.len() != right_state.stack.len() {
            let len = current.stack.len().min(right_state.stack.len());
            current.stack.truncate(len);
        }
        for (left, right) in current.stack.iter_mut().zip(&right_state.stack) {
            if left.ty != right.ty {
                left.ty = merge_type(&left.ty, &right.ty);
            }
            if format!("{:?}", left.expr) != format!("{:?}", right.expr) {
                left.expr = merge_expr(&left.expr, &right.expr)
                    .or_else(|| {
                        merge_branch_values(
                            entries,
                            cfg,
                            context,
                            block,
                            (&previous_sources, &left.expr),
                            (*right_source, &right.expr),
                        )
                    })
                    .unwrap_or(Expr::Opaque { opcode: 0, offset });
            }
        }
        for (slot, ty, name) in &right_state.local_types {
            if let Some(existing) = current.local_types.iter_mut().find(|(s, _, _)| s == slot) {
                existing.1 = merge_type(&existing.1, ty);
                if existing.2.is_none() {
                    existing.2.clone_from(name);
                }
            } else {
                current.local_types.push((*slot, ty.clone(), name.clone()));
            }
        }
        previous_sources.push(*right_source);
    }
    for (index, expression) in switch_values {
        if let Some(slot) = current.stack.get_mut(index) {
            slot.expr = expression;
        }
    }
    current.local_values = merge_local_values(cfg, block, &contributions);
    let after = state_fingerprint(&current);
    let changed = before.as_deref() != Some(after.as_str());
    entries.insert(block, current);
    changed
}

fn merge_local_values(
    cfg: &Cfg,
    block: BlockId,
    contributions: &[(BlockId, BlockEntryState)],
) -> HashMap<u16, Expr> {
    let target_offset = cfg.block(block).start_offset;
    let forward = contributions
        .iter()
        .filter(|(source, _)| cfg.block(*source).start_offset < target_offset)
        .collect::<Vec<_>>();
    let has_backedge = forward.len() < contributions.len();
    let relevant = if has_backedge && !forward.is_empty() {
        forward
    } else {
        contributions.iter().collect()
    };
    let slots = relevant
        .iter()
        .flat_map(|(_, state)| state.local_values.keys().copied())
        .collect::<HashSet<_>>();
    let mut merged = HashMap::new();
    for slot in slots {
        let values = relevant
            .iter()
            .map(|(_, state)| state.local_values.get(&slot))
            .collect::<Vec<_>>();
        let Some(Some(first)) = values.first() else {
            continue;
        };
        let fingerprint = format!("{:?}", first);
        if values
            .iter()
            .all(|value| value.is_some_and(|value| format!("{:?}", value) == fingerprint))
        {
            merged.insert(slot, (*first).clone());
        }
    }
    merged
}

fn recover_switch_entry_values(
    entries: &HashMap<BlockId, BlockEntryState>,
    cfg: &Cfg,
    context: &RenderContext<'_>,
    join: BlockId,
    contributions: &[(BlockId, BlockEntryState)],
) -> HashMap<usize, Expr> {
    let mut recovered = HashMap::new();
    let Some(stack_len) = contributions
        .iter()
        .map(|(_, state)| state.stack.len())
        .min()
    else {
        return recovered;
    };
    let contribution_refs = contributions
        .iter()
        .map(|(source, state)| (*source, state))
        .collect::<Vec<_>>();
    for stack_index in 0..stack_len {
        let first = format!("{:?}", contributions[0].1.stack[stack_index].expr);
        if contributions
            .iter()
            .all(|(_, state)| format!("{:?}", state.stack[stack_index].expr) == first)
        {
            continue;
        }
        if let Some(expression) = recover_switch_value(
            entries,
            cfg,
            context,
            join,
            &contribution_refs,
            stack_index,
            &HashSet::new(),
        ) {
            recovered.insert(stack_index, expression);
        }
    }
    recovered
}

fn recover_switch_value(
    entries: &HashMap<BlockId, BlockEntryState>,
    cfg: &Cfg,
    context: &RenderContext<'_>,
    join: BlockId,
    contributions: &[(BlockId, &BlockEntryState)],
    stack_index: usize,
    excluded_heads: &HashSet<BlockId>,
) -> Option<Expr> {
    use crate::classfile::instruction::InsnKind;

    for head in cfg.real_blocks() {
        if excluded_heads.contains(&head.id) {
            continue;
        }
        let Some(switch_index) = head.instructions.iter().rposition(|instruction| {
            matches!(
                instruction.kind,
                InsnKind::TableSwitch { .. } | InsnKind::LookupSwitch { .. }
            )
        }) else {
            continue;
        };
        let switch = &head.instructions[switch_index];
        let mut targets = Vec::<(Option<i32>, BlockId)>::new();
        match &switch.kind {
            InsnKind::TableSwitch {
                default_offset,
                low,
                offsets,
                ..
            } => {
                for (index, offset) in offsets.iter().enumerate() {
                    if let Some(block) =
                        block_at_offset(cfg, (i64::from(switch.offset) + i64::from(*offset)) as u32)
                    {
                        targets.push((Some(*low + index as i32), block));
                    }
                }
                if let Some(block) = block_at_offset(
                    cfg,
                    (i64::from(switch.offset) + i64::from(*default_offset)) as u32,
                ) {
                    targets.push((None, block));
                }
            }
            InsnKind::LookupSwitch {
                default_offset,
                pairs,
            } => {
                for (value, offset) in pairs {
                    if let Some(block) =
                        block_at_offset(cfg, (i64::from(switch.offset) + i64::from(*offset)) as u32)
                    {
                        targets.push((Some(*value), block));
                    }
                }
                if let Some(block) = block_at_offset(
                    cfg,
                    (i64::from(switch.offset) + i64::from(*default_offset)) as u32,
                ) {
                    targets.push((None, block));
                }
            }
            _ => continue,
        }

        let mapped = targets
            .iter()
            .filter_map(|(value, target)| {
                let states = contributions
                    .iter()
                    .filter(|(source, _)| {
                        branch_distance(cfg, head.id, *target, *source, join).is_some()
                    })
                    .map(|(source, state)| (*source, *state))
                    .collect::<Vec<_>>();
                (!states.is_empty()).then_some((*value, states))
            })
            .collect::<Vec<_>>();
        let mapped_sources = mapped
            .iter()
            .flat_map(|(_, states)| states.iter().map(|(source, _)| *source))
            .collect::<HashSet<_>>();
        if mapped.len() < 2 {
            continue;
        }

        let entry = entries.get(&head.id).cloned().unwrap_or_default();
        let selector = context.simulate_state(&head.instructions[..switch_index], &entry);
        let Some(selector) = selector.stack_out.last().map(|slot| slot.expr.clone()) else {
            continue;
        };
        let mut child_excluded = excluded_heads.clone();
        child_excluded.insert(head.id);
        let arms = mapped
            .iter()
            .map(|(value, states)| {
                merge_switch_arm_value(
                    entries,
                    cfg,
                    context,
                    join,
                    states,
                    stack_index,
                    &child_excluded,
                )
                .map(|expression| (*value, expression))
            })
            .collect::<Option<Vec<_>>>();
        if let Some(arms) = arms {
            let mut expression = Expr::SwitchExpression {
                selector: Box::new(selector),
                arms,
            };
            let mut merged_sources = contributions
                .iter()
                .filter(|(source, _)| mapped_sources.contains(source))
                .map(|(source, _)| *source)
                .collect::<Vec<_>>();
            let mut complete = true;
            for (source, state) in contributions
                .iter()
                .filter(|(source, _)| !mapped_sources.contains(source))
            {
                let Some(right) = state.stack.get(stack_index).map(|slot| &slot.expr) else {
                    complete = false;
                    break;
                };
                if format!("{:?}", expression) != format!("{:?}", right) {
                    let Some(merged) = merge_expr(&expression, right).or_else(|| {
                        merge_branch_values(
                            entries,
                            cfg,
                            context,
                            join,
                            (&merged_sources, &expression),
                            (*source, right),
                        )
                    }) else {
                        complete = false;
                        break;
                    };
                    expression = merged;
                }
                merged_sources.push(*source);
            }
            if complete {
                return Some(expression);
            }
        }
    }
    None
}

fn merge_switch_arm_value(
    entries: &HashMap<BlockId, BlockEntryState>,
    cfg: &Cfg,
    context: &RenderContext<'_>,
    join: BlockId,
    states: &[(BlockId, &BlockEntryState)],
    stack_index: usize,
    excluded_heads: &HashSet<BlockId>,
) -> Option<Expr> {
    let (first_source, first_state) = states.first()?;
    let mut expression = first_state.stack.get(stack_index)?.expr.clone();
    let mut merged_sources = vec![*first_source];
    for (source, state) in states.iter().skip(1) {
        let right = &state.stack.get(stack_index)?.expr;
        if format!("{:?}", expression) != format!("{:?}", right) {
            let Some(merged) = merge_expr(&expression, right).or_else(|| {
                merge_branch_values(
                    entries,
                    cfg,
                    context,
                    join,
                    (&merged_sources, &expression),
                    (*source, right),
                )
            }) else {
                return recover_switch_value(
                    entries,
                    cfg,
                    context,
                    join,
                    states,
                    stack_index,
                    excluded_heads,
                );
            };
            expression = merged;
        }
        merged_sources.push(*source);
    }
    Some(expression)
}

fn block_at_offset(cfg: &Cfg, offset: u32) -> Option<BlockId> {
    cfg.real_blocks()
        .find(|block| block.start_offset == offset)
        .map(|block| block.id)
}

fn merge_branch_values(
    entries: &HashMap<BlockId, BlockEntryState>,
    cfg: &Cfg,
    context: &RenderContext<'_>,
    join: BlockId,
    left: (&[BlockId], &Expr),
    right: (BlockId, &Expr),
) -> Option<Expr> {
    let (left_sources, left) = left;
    let (right_source, right) = right;
    let (head, left_when_taken) = left_sources
        .iter()
        .filter(|source| **source != right_source)
        .filter_map(|source| find_value_branch(cfg, join, *source, right_source))
        .min_by_key(|(head, _)| branch_nesting_score(cfg, *head, join))?;
    let block = cfg.block(head);
    let branch_index = block
        .instructions
        .iter()
        .rposition(|instruction| is_conditional_branch(instruction.opcode))?;
    let branch = &block.instructions[branch_index];
    let entry = entries.get(&head).cloned().unwrap_or_default();
    let result = context.simulate_state(&block.instructions[..branch_index], &entry);
    let condition = render_branch_condition(branch.opcode, &result.stack_out)?;
    let (then_expr, else_expr) = if left_when_taken {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    };
    Some(Expr::Ternary {
        cond: TernaryCondition::Expression(Box::new(condition)),
        then_expr: Box::new(then_expr),
        else_expr: Box::new(else_expr),
    })
}

fn branch_nesting_score(cfg: &Cfg, head: BlockId, join: BlockId) -> usize {
    let mut queue = std::collections::VecDeque::from([(head, 0usize)]);
    let mut visited = HashSet::new();
    while let Some((block, distance)) = queue.pop_front() {
        if block == join {
            return distance;
        }
        if block == EXIT_BLOCK || !visited.insert(block) {
            continue;
        }
        for &successor in &cfg.block(block).succs {
            queue.push_back((successor, distance + 1));
        }
    }
    usize::MAX
}

fn find_value_branch(
    cfg: &Cfg,
    join: BlockId,
    left_source: BlockId,
    right_source: BlockId,
) -> Option<(BlockId, bool)> {
    let mut best = None::<(usize, BlockId, bool)>;
    for candidate in cfg.real_blocks() {
        if candidate.succs.len() != 2
            || !candidate
                .last_insn()
                .is_some_and(|instruction| is_conditional_branch(instruction.opcode))
        {
            continue;
        }
        let taken = candidate.succs[0];
        let fallthrough = candidate.succs[1];
        for (left_when_taken, left_start, right_start) in
            [(true, taken, fallthrough), (false, fallthrough, taken)]
        {
            let Some(left_distance) =
                branch_distance(cfg, candidate.id, left_start, left_source, join)
            else {
                continue;
            };
            let Some(right_distance) =
                branch_distance(cfg, candidate.id, right_start, right_source, join)
            else {
                continue;
            };
            let score = left_distance + right_distance;
            if best.is_none_or(|(best_score, _, _)| score < best_score) {
                best = Some((score, candidate.id, left_when_taken));
            }
        }
    }
    best.map(|(_, head, left_when_taken)| (head, left_when_taken))
}

fn branch_distance(
    cfg: &Cfg,
    head: BlockId,
    start: BlockId,
    source: BlockId,
    join: BlockId,
) -> Option<usize> {
    if source == head {
        return (start == join).then_some(0);
    }
    let mut queue = std::collections::VecDeque::from([(start, 0usize)]);
    let mut visited = HashSet::new();
    while let Some((block, distance)) = queue.pop_front() {
        if block == source {
            return Some(distance);
        }
        if block == join || block == EXIT_BLOCK || !visited.insert(block) {
            continue;
        }
        for &successor in &cfg.block(block).succs {
            queue.push_back((successor, distance + 1));
        }
    }
    None
}

fn render_branch_condition(opcode: u8, stack: &[SlotInfo]) -> Option<Expr> {
    use crate::classfile::opcodes::opc;

    let top = stack.last()?;
    if top.ty == JavaType::BOOLEAN {
        return Some(match opcode {
            opc::ifeq => Expr::UnOp(UnOp::BoolNot, Box::new(top.expr.clone())),
            opc::ifne => top.expr.clone(),
            _ => top.expr.clone(),
        });
    }
    let zero = || {
        Expr::Const(ConstExpr {
            value: ConstValue::Int(0),
            ty: JavaType::INT,
        })
    };
    let second = || {
        stack
            .get(stack.len().saturating_sub(2))
            .map(|value| value.expr.clone())
    };
    let (operator, left, right) = match opcode {
        opc::ifeq => (BinOp::Eq, top.expr.clone(), zero()),
        opc::ifne => (BinOp::Ne, top.expr.clone(), zero()),
        opc::iflt => (BinOp::Lt, top.expr.clone(), zero()),
        opc::ifge => (BinOp::Ge, top.expr.clone(), zero()),
        opc::ifgt => (BinOp::Gt, top.expr.clone(), zero()),
        opc::ifle => (BinOp::Le, top.expr.clone(), zero()),
        opc::if_icmpeq | opc::if_acmpeq => (BinOp::Eq, second()?, top.expr.clone()),
        opc::if_icmpne | opc::if_acmpne => (BinOp::Ne, second()?, top.expr.clone()),
        opc::if_icmplt => (BinOp::Lt, second()?, top.expr.clone()),
        opc::if_icmpge => (BinOp::Ge, second()?, top.expr.clone()),
        opc::if_icmpgt => (BinOp::Gt, second()?, top.expr.clone()),
        opc::if_icmple => (BinOp::Le, second()?, top.expr.clone()),
        opc::ifnull => (BinOp::Eq, top.expr.clone(), Expr::Null),
        opc::ifnonnull => (BinOp::Ne, top.expr.clone(), Expr::Null),
        _ => return None,
    };
    Some(Expr::BinOp(operator, Box::new(left), Box::new(right)))
}

fn is_conditional_branch(opcode: u8) -> bool {
    use crate::classfile::opcodes::opc;
    matches!(
        opcode,
        opc::ifeq
            | opc::ifne
            | opc::iflt
            | opc::ifge
            | opc::ifgt
            | opc::ifle
            | opc::if_icmpeq
            | opc::if_icmpne
            | opc::if_icmplt
            | opc::if_icmpge
            | opc::if_icmpgt
            | opc::if_icmple
            | opc::if_acmpeq
            | opc::if_acmpne
            | opc::ifnull
            | opc::ifnonnull
    )
}

fn merge_expr(left: &Expr, right: &Expr) -> Option<Expr> {
    fn is_result_local(expr: &Expr) -> bool {
        matches!(expr, Expr::LocalVar(local) if local.name.as_deref().is_some_and(|name| name == "$result" || name == "result"))
    }
    fn is_suspend_call(expr: &Expr) -> bool {
        matches!(expr, Expr::Invoke { descriptor, .. } if MethodDescriptor::parse(descriptor)
            .ok()
            .is_some_and(|method| method.params.iter().any(|ty| ty.class_name.as_deref() == Some("kotlin/coroutines/Continuation"))))
    }
    if is_suspend_call(left) && is_result_local(right) {
        Some(left.clone())
    } else if is_suspend_call(right) && is_result_local(left) {
        Some(right.clone())
    } else {
        None
    }
}

fn merge_type(left: &JavaType, right: &JavaType) -> JavaType {
    if left == right {
        left.clone()
    } else if left.is_reference() && right.is_reference() {
        JavaType::object("java/lang/Object")
    } else {
        JavaType::UNKNOWN
    }
}

fn state_fingerprint(state: &BlockEntryState) -> String {
    let mut local_values = state
        .local_values
        .iter()
        .map(|(slot, value)| (*slot, format!("{:?}", value)))
        .collect::<Vec<_>>();
    local_values.sort_by_key(|(slot, _)| *slot);
    format!(
        "{:?}|{:?}|{:?}",
        state.stack, state.local_types, local_values
    )
}

fn concat_recipes(class: &ClassFile) -> HashMap<u16, String> {
    let mut recipes = HashMap::new();
    for attribute in &class.attributes {
        if let Attribute::BootstrapMethods(methods) = attribute {
            for (index, method) in methods.iter().enumerate() {
                if let Some(&constant_index) = method.arguments.first() {
                    if let Ok(CpEntry::String(recipe)) = class.constant_pool.get(constant_index) {
                        recipes.insert(index as u16, recipe.clone());
                    }
                }
            }
        }
    }
    recipes
}

fn detects_coroutine_state_machine(code: &CodeAttribute, class: &ClassFile) -> bool {
    code.instructions.iter().any(|instruction| {
        let crate::classfile::instruction::InsnKind::Invoke { index, .. } = instruction.kind else {
            return false;
        };
        matches!(
            class.constant_pool.get(index),
            Ok(CpEntry::Methodref(member) | CpEntry::InterfaceMethodref(member))
                if member.class_name == "kotlin/coroutines/intrinsics/IntrinsicsKt"
                    && member.name == "getCOROUTINE_SUSPENDED"
        )
    })
}

fn invocation_name<'a>(instruction: &Instruction, pool: &'a ConstantPool) -> Option<&'a str> {
    let crate::classfile::instruction::InsnKind::Invoke { index, .. } = instruction.kind else {
        return None;
    };
    match pool.get(index).ok()? {
        CpEntry::Methodref(member) | CpEntry::InterfaceMethodref(member) => Some(&member.name),
        _ => None,
    }
}

fn expression_is_local_slot(expression: &Expr, slot: u16) -> bool {
    match expression {
        Expr::LocalVar(local) => local.slot == slot,
        Expr::Cast(_, _, inner) => expression_is_local_slot(inner, slot),
        _ => false,
    }
}

fn expression_has_type(expression: &Expr, expected: &JavaType) -> bool {
    match expression {
        Expr::LocalVar(local) => &local.ty == expected,
        Expr::Cast(_, ty, _) => ty == expected,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lvt(slot: u16, name: &str, descriptor: &str, start_pc: u16) -> LvtEntry {
        LvtEntry {
            slot,
            name: name.into(),
            descriptor: descriptor.into(),
            start_pc,
            length: 10,
        }
    }

    #[test]
    fn lvt_names_are_unique_across_distinct_local_identities() {
        let mut entries = vec![
            lvt(2, "this_$iv", "Lpkg/Slots;", 10),
            lvt(3, "this_$iv", "Lpkg/Slots;", 133),
            lvt(4, "this_$iv", "Lpkg/Owner;", 242),
        ];

        disambiguate_lvt_names(&mut entries);

        assert_eq!(entries[0].name, "this_$iv");
        assert_eq!(entries[1].name, "this_$iv_3");
        assert_eq!(entries[2].name, "this_$iv_4");
    }

    #[test]
    fn split_lvt_ranges_keep_the_same_identity_name() {
        let mut entries = vec![
            lvt(2, "value", "Ljava/lang/String;", 10),
            lvt(2, "value", "Ljava/lang/String;", 40),
            lvt(3, "value_3", "Ljava/lang/Object;", 0),
        ];

        disambiguate_lvt_names(&mut entries);

        assert_eq!(entries[0].name, "value");
        assert_eq!(entries[1].name, "value");
        assert_eq!(entries[2].name, "value_3");
    }
}
