#!/usr/bin/env python3
"""让 Alpine 构建工具能够加载 libclang，不改变目标静态产物的 CRT。"""
import os
import sys


compiler, *arguments = sys.argv[1:]
# 显式 --target 将目标编译与 host build-script/proc-macro 分开；
# musl 默认静态链接的 host 可执行文件无法执行 bindgen 的 dlopen。
if "--crate-name" in arguments and not any(
    argument == "--target" or argument.startswith("--target=") for argument in arguments
):
    arguments.extend(["-C", "target-feature=-crt-static"])
os.execv(compiler, [compiler, *arguments])
