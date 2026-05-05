use core::arch::global_asm;

global_asm!(
    r#"
    .section .text,"ax",@progbits

    .globl _start
    .type _start,@function
_start:
    movq %rsp, %rbx
    movq %rbx, %rdi
    leaq _DYNAMIC(%rip), %rsi
    andq $-16, %rsp
    call rtld_bootstrap
    movq %rbx, %rsp
    xorq %rdx, %rdx
    jmp *%rax
    .size _start, . - _start
"#,
    options(att_syntax)
);
