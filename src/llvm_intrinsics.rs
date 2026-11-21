//! A closed set of compiler-provided LLVM intrinsics. Not user-supplied IR.
//! The type checker validates each std::abi::llvm declaration before MIR is built.
//! These are *defined* in the generated LLVM module, not linked from libc/C.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Intrinsic {
    CopyBytes, MoveBytes, SetBytes, Popcount64, LeadingZeros64,
    TrailingZeros64, ByteSwap64, ReverseBits64,
    SqrtF64, AbsF64, FloorF64, CeilF64,
    HeapAllocate, HeapFree, PointerOffset,
    TextLength, TextAddress, WriteBytes, ProcessExit, Errno, ClockGetTime, OpenAt, CloseFd, LoadByte,
    ThreadStart, ThreadStartArg, ThreadJoin, ThreadYield, ThreadCurrent, SleepMillis,
    WinStdHandle, WinWriteFile, WinExitProcess,
    NetSocket, NetSetSockOpt, NetBind, NetListen, NetAccept, NetRecv, NetSend,
    NetGetFlags, NetSetFlags, NetConnect, NetResolve, NetFreeAddresses, NetGetSockOpt, NetPoll, ReadFd, GetEnvAddress, BorrowText, GetRandom,
    AtomicLoad, AtomicStore, AtomicSwap, AtomicFetchAdd, AtomicFetchSub, AtomicFetchAnd, AtomicFetchOr, AtomicFetchXor, AtomicCompareExchange,
    ProcessOpen, ProcessReadLine, ProcessEof, ProcessClose,
}
impl Intrinsic {
    pub const ALL: [Self; 64] = [
        Self::CopyBytes, Self::MoveBytes, Self::SetBytes, Self::Popcount64,
        Self::LeadingZeros64, Self::TrailingZeros64, Self::ByteSwap64,
        Self::ReverseBits64, Self::SqrtF64, Self::AbsF64, Self::FloorF64, Self::CeilF64,
        Self::HeapAllocate, Self::HeapFree, Self::PointerOffset,
        Self::TextLength, Self::TextAddress, Self::WriteBytes, Self::ProcessExit, Self::Errno,
        Self::ClockGetTime, Self::OpenAt, Self::CloseFd, Self::LoadByte,
        Self::ThreadStart, Self::ThreadStartArg, Self::ThreadJoin, Self::ThreadYield, Self::ThreadCurrent, Self::SleepMillis,
        Self::WinStdHandle, Self::WinWriteFile, Self::WinExitProcess,
        Self::NetSocket, Self::NetSetSockOpt, Self::NetBind, Self::NetListen,
        Self::NetAccept, Self::NetRecv, Self::NetSend, Self::NetGetFlags, Self::NetSetFlags,
        Self::NetConnect, Self::NetResolve, Self::NetFreeAddresses, Self::NetGetSockOpt, Self::NetPoll,
        Self::ReadFd, Self::GetEnvAddress, Self::BorrowText, Self::GetRandom,
        Self::AtomicLoad, Self::AtomicStore, Self::AtomicSwap, Self::AtomicFetchAdd, Self::AtomicFetchSub, Self::AtomicFetchAnd, Self::AtomicFetchOr, Self::AtomicFetchXor, Self::AtomicCompareExchange,
        Self::ProcessOpen, Self::ProcessReadLine, Self::ProcessEof, Self::ProcessClose,
    ];
    pub fn from_name(name: &str) -> Option<Self> {
        // Surface lowering flattens std::abi::llvm::foo to std__abi__llvm__foo.
        let short = name.strip_prefix("std__abi__llvm__").or_else(||
            name.rsplit_once("__std__abi__llvm__").map(|(_, suffix)| suffix))?;
        match short {
            "copy_bytes" => Some(Self::CopyBytes),
            "move_bytes" => Some(Self::MoveBytes),
            "set_bytes" => Some(Self::SetBytes),
            "popcount64" => Some(Self::Popcount64),
            "leading_zeros64" => Some(Self::LeadingZeros64),
            "trailing_zeros64" => Some(Self::TrailingZeros64),
            "byte_swap64" => Some(Self::ByteSwap64),
            "reverse_bits64" => Some(Self::ReverseBits64),
            "square_root_f64" => Some(Self::SqrtF64),
            "absolute_f64" => Some(Self::AbsF64),
            "floor_f64" => Some(Self::FloorF64),
            "ceil_f64" => Some(Self::CeilF64),
            "heap_allocate" => Some(Self::HeapAllocate),
            "heap_free" => Some(Self::HeapFree),
            "pointer_offset" => Some(Self::PointerOffset),
            "text_length" => Some(Self::TextLength),
            "text_address" => Some(Self::TextAddress),
            "write_bytes" => Some(Self::WriteBytes),
            "process_exit" => Some(Self::ProcessExit),
            "last_errno" => Some(Self::Errno),
            "clock_gettime" => Some(Self::ClockGetTime),
            "open_at" => Some(Self::OpenAt),
            "close_fd" => Some(Self::CloseFd),
            "load_byte" => Some(Self::LoadByte),
            "thread_start" => Some(Self::ThreadStart),
            "thread_start_arg" => Some(Self::ThreadStartArg),
            "thread_join" => Some(Self::ThreadJoin),
            "thread_yield" => Some(Self::ThreadYield),
            "thread_current" => Some(Self::ThreadCurrent),
            "sleep_millis" => Some(Self::SleepMillis),
            "win_std_handle" => Some(Self::WinStdHandle),
            "win_write_file" => Some(Self::WinWriteFile),
            "win_exit_process" => Some(Self::WinExitProcess),
            "net_socket" => Some(Self::NetSocket),
            "net_set_option" => Some(Self::NetSetSockOpt),
            "net_bind" => Some(Self::NetBind),
            "net_listen" => Some(Self::NetListen),
            "net_accept" => Some(Self::NetAccept),
            "net_recv" => Some(Self::NetRecv),
            "net_send" => Some(Self::NetSend),
            "net_get_flags" => Some(Self::NetGetFlags),
            "net_set_flags" => Some(Self::NetSetFlags),
            "net_connect" => Some(Self::NetConnect),
            "net_resolve" => Some(Self::NetResolve),
            "net_free_addresses" => Some(Self::NetFreeAddresses),
            "net_get_option" => Some(Self::NetGetSockOpt),
            "net_poll" => Some(Self::NetPoll),
            "read_fd" => Some(Self::ReadFd),
            "getenv_address" => Some(Self::GetEnvAddress),
            "borrow_text" => Some(Self::BorrowText),
            "random_bytes" => Some(Self::GetRandom),
            "atomic_load_i64" => Some(Self::AtomicLoad),
            "atomic_store_i64" => Some(Self::AtomicStore),
            "atomic_swap_i64" => Some(Self::AtomicSwap),
            "atomic_fetch_add_i64" => Some(Self::AtomicFetchAdd),
            "atomic_fetch_sub_i64" => Some(Self::AtomicFetchSub),
            "atomic_fetch_and_i64" => Some(Self::AtomicFetchAnd),
            "atomic_fetch_or_i64" => Some(Self::AtomicFetchOr),
            "atomic_fetch_xor_i64" => Some(Self::AtomicFetchXor),
            "atomic_compare_exchange_i64" => Some(Self::AtomicCompareExchange),
            "process_open" => Some(Self::ProcessOpen),
            "process_read_line" => Some(Self::ProcessReadLine),
            "process_eof" => Some(Self::ProcessEof),
            "process_close" => Some(Self::ProcessClose),

            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::CopyBytes => "memcpy", Self::MoveBytes => "memmove",
            Self::SetBytes => "memset", Self::Popcount64 => "ctpop",
            Self::LeadingZeros64 => "ctlz", Self::TrailingZeros64 => "cttz",
            Self::ByteSwap64 => "bswap", Self::ReverseBits64 => "bitreverse",
            Self::SqrtF64 => "sqrt", Self::AbsF64 => "fabs",
            Self::FloorF64 => "floor", Self::CeilF64 => "ceil",
            Self::HeapAllocate => "aligned_alloc",
            Self::HeapFree => "free",
            Self::PointerOffset => "gep",
            Self::TextLength => "strlen",
            Self::TextAddress => "ptrtoint",
            Self::WriteBytes => "write",
            Self::ProcessExit => "_Exit",
            Self::Errno => "errno",
            Self::ClockGetTime => "clock_gettime",
            Self::OpenAt => "openat",
            Self::CloseFd => "close",
            Self::LoadByte => "load8",
            Self::ThreadStart => "pthread_create",
            Self::ThreadStartArg => "pthread_create_arg",
            Self::ThreadJoin => "pthread_join",
            Self::ThreadYield => "sched_yield",
            Self::ThreadCurrent => "pthread_self",
            Self::SleepMillis => "nanosleep",
            Self::WinStdHandle => "GetStdHandle",
            Self::WinWriteFile => "WriteFile",
            Self::WinExitProcess => "ExitProcess",
            Self::NetSocket => "socket", Self::NetSetSockOpt => "setsockopt",
            Self::NetBind => "bind", Self::NetListen => "listen", Self::NetAccept => "accept",
            Self::NetRecv => "recv", Self::NetSend => "send",
            Self::NetGetFlags => "fcntl_getfl", Self::NetSetFlags => "fcntl_setfl",
            Self::NetConnect => "connect", Self::NetResolve => "getaddrinfo",
            Self::NetFreeAddresses => "freeaddrinfo", Self::NetGetSockOpt => "getsockopt", Self::NetPoll => "poll",
            Self::ReadFd => "read", Self::GetEnvAddress => "getenv", Self::BorrowText => "inttoptr",
            Self::GetRandom => "getrandom",
            Self::AtomicLoad => "atomic_load_i64",
            Self::AtomicStore => "atomic_store_i64",
            Self::AtomicSwap => "atomic_swap_i64",
            Self::AtomicFetchAdd => "atomic_fetch_add_i64",
            Self::AtomicFetchSub => "atomic_fetch_sub_i64",
            Self::AtomicFetchAnd => "atomic_fetch_and_i64",
            Self::AtomicFetchOr => "atomic_fetch_or_i64",
            Self::AtomicFetchXor => "atomic_fetch_xor_i64",
            Self::AtomicCompareExchange => "atomic_compare_exchange_i64",
            Self::ProcessOpen => "popen",
            Self::ProcessReadLine => "fgets",
            Self::ProcessEof => "feof",
            Self::ProcessClose => "pclose",

        }
    }
    pub fn signature(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::CopyBytes | Self::MoveBytes => ("void", &["ptr", "ptr", "i64"]),
            Self::SetBytes => ("void", &["ptr", "i8", "i64"]),
            Self::Popcount64 | Self::LeadingZeros64 | Self::TrailingZeros64 |
            Self::ByteSwap64 | Self::ReverseBits64 => ("i64", &["i64"]),
            Self::SqrtF64 | Self::AbsF64 | Self::FloorF64 | Self::CeilF64 =>
                ("double", &["double"]),
            Self::HeapAllocate => ("i64", &["i64", "i64"]),
            Self::HeapFree => ("void", &["i64"]),
            Self::PointerOffset => ("i64", &["i64", "i64"]),
            Self::TextLength | Self::TextAddress => ("i64", &["ptr"]),
            Self::WriteBytes => ("i64", &["i32", "i64", "i64"]),
            Self::ProcessExit => ("void", &["i32"]),
            Self::Errno => ("i32", &[]),
            Self::ClockGetTime => ("i32", &["i32", "ptr"]),
            Self::OpenAt => ("i32", &["i32", "ptr", "i32", "i32"]),
            Self::CloseFd => ("i32", &["i32"]),
            Self::LoadByte => ("i8", &["i64"]),
            Self::ThreadStart => ("i32", &["ptr", "ptr"]),
            Self::ThreadStartArg => ("i32", &["ptr", "ptr", "ptr"]),
            Self::ThreadJoin => ("i32", &["i64"]),
            Self::ThreadYield => ("i32", &[]),
            Self::SleepMillis => ("i32", &["i64"]),
            Self::ThreadCurrent => ("i64", &[]),
            Self::WinStdHandle => ("i64", &["i32"]),
            Self::WinWriteFile => ("i32", &["i64", "i64", "i32", "ptr"]),
            Self::WinExitProcess => ("void", &["i32"]),
            Self::NetSocket => ("i32", &["i32", "i32", "i32"]),
            Self::NetSetSockOpt => ("i32", &["i32", "i32", "i32", "ptr", "i32"]),
            Self::NetBind => ("i32", &["i32", "ptr", "i32"]),
            Self::NetListen => ("i32", &["i32", "i32"]),
            Self::NetAccept => ("i32", &["i32"]),
            Self::NetRecv | Self::NetSend => ("i64", &["i32", "ptr", "i64", "i32"]),
            Self::NetGetFlags => ("i32", &["i32", "i32"]),
            Self::NetSetFlags => ("i32", &["i32", "i32", "i32"]),
            Self::NetConnect => ("i32", &["i32", "ptr", "i32"]),
            Self::NetResolve => ("i32", &["ptr", "ptr", "ptr", "ptr"]),
            Self::NetFreeAddresses => ("void", &["ptr"]),
            Self::NetGetSockOpt => ("i32", &["i32", "i32", "i32", "ptr", "ptr"]),
            Self::NetPoll => ("i32", &["ptr", "i64", "i32"]),
            Self::ReadFd => ("i64", &["i32", "ptr", "i64"]),
            Self::GetEnvAddress => ("i64", &["ptr"]),
            Self::BorrowText => ("ptr", &["i64"]),
            Self::GetRandom => ("i64", &["ptr", "i64", "i32"]),
            Self::AtomicLoad => ("i64", &["ptr", "i32"]),
            Self::AtomicStore => ("void", &["ptr", "i64", "i32"]),
            Self::AtomicSwap | Self::AtomicFetchAdd | Self::AtomicFetchSub |
            Self::AtomicFetchAnd | Self::AtomicFetchOr | Self::AtomicFetchXor => ("i64", &["ptr", "i64", "i32"]),
            Self::AtomicCompareExchange => ("i32", &["ptr", "i64", "i64", "i32", "i32"]),
            Self::ProcessOpen => ("i64", &["ptr", "ptr"]),
            Self::ProcessReadLine => ("i64", &["i64", "i32", "i64"]),
            Self::ProcessEof | Self::ProcessClose => ("i32", &["i64"]),
        }
    }
    pub fn is_atomic(self) -> bool {
        matches!(self, Self::AtomicLoad | Self::AtomicStore | Self::AtomicSwap |
            Self::AtomicFetchAdd | Self::AtomicFetchSub | Self::AtomicFetchAnd |
            Self::AtomicFetchOr | Self::AtomicFetchXor | Self::AtomicCompareExchange)
    }
    pub fn is_unsafe(self) -> bool {
        matches!(self, Self::CopyBytes | Self::MoveBytes | Self::SetBytes |
            Self::HeapAllocate | Self::HeapFree | Self::PointerOffset |
            Self::WriteBytes | Self::ProcessExit | Self::ClockGetTime |
            Self::OpenAt | Self::CloseFd | Self::LoadByte |
            Self::ThreadStart | Self::ThreadStartArg | Self::ThreadJoin |
            Self::WinWriteFile | Self::WinExitProcess |
            Self::NetSocket | Self::NetSetSockOpt | Self::NetBind | Self::NetListen |
            Self::NetAccept | Self::NetRecv | Self::NetSend |
            Self::NetGetFlags | Self::NetSetFlags | Self::NetConnect | Self::NetResolve |
            Self::NetFreeAddresses | Self::NetGetSockOpt | Self::NetPoll | Self::ReadFd | Self::BorrowText | Self::GetRandom |
            Self::AtomicLoad | Self::AtomicStore | Self::AtomicSwap |
            Self::AtomicFetchAdd | Self::AtomicFetchSub | Self::AtomicFetchAnd |
            Self::AtomicFetchOr | Self::AtomicFetchXor | Self::AtomicCompareExchange |
            Self::ProcessOpen | Self::ProcessReadLine | Self::ProcessEof | Self::ProcessClose)
    }
    pub fn declaration(self) -> &'static str {
        match self {
            Self::CopyBytes => "declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)\n",
            Self::MoveBytes => "declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1)\n",
            Self::SetBytes => "declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)\n",
            Self::Popcount64 => "declare i64 @llvm.ctpop.i64(i64)\n",
            Self::LeadingZeros64 => "declare i64 @llvm.ctlz.i64(i64, i1)\n",
            Self::TrailingZeros64 => "declare i64 @llvm.cttz.i64(i64, i1)\n",
            Self::ByteSwap64 => "declare i64 @llvm.bswap.i64(i64)\n",
            Self::ReverseBits64 => "declare i64 @llvm.bitreverse.i64(i64)\n",
            Self::SqrtF64 => "declare double @llvm.sqrt.f64(double)\n",
            Self::AbsF64 => "declare double @llvm.fabs.f64(double)\n",
            Self::FloorF64 => "declare double @llvm.floor.f64(double)\n",
            Self::CeilF64 => "declare double @llvm.ceil.f64(double)\n",
            Self::HeapAllocate => "declare ptr @aligned_alloc(i64, i64)\n",
            Self::HeapFree => "declare void @free(ptr)\n",
            Self::PointerOffset | Self::TextLength | Self::TextAddress => "",
            Self::WriteBytes => "declare i64 @write(i32, ptr, i64)\n",
            Self::ProcessExit => "declare void @_Exit(i32)\n",
            Self::Errno => "declare ptr @__errno_location()\n",
            Self::ClockGetTime => "declare i32 @clock_gettime(i32, ptr)\n",
            Self::OpenAt => "declare i32 @openat(i32, ptr, i32, i32)\n",
            Self::CloseFd => "declare i32 @close(i32)\n",
            Self::LoadByte => "",
            Self::ThreadStart | Self::ThreadStartArg => "declare i32 @pthread_create(ptr, ptr, ptr, ptr)\n",
            Self::ThreadJoin => "declare i32 @pthread_join(i64, ptr)\n",
            Self::ThreadYield => "declare i32 @sched_yield()\n",
            Self::SleepMillis => "declare i32 @nanosleep(ptr, ptr)\n",
            Self::ThreadCurrent => "declare i64 @pthread_self()\n",
            Self::WinStdHandle => "declare ptr @GetStdHandle(i32)\n",
            Self::WinWriteFile => "declare i32 @WriteFile(ptr, ptr, i32, ptr, ptr)\n",
            Self::WinExitProcess => "declare void @ExitProcess(i32)\n",
            Self::NetSocket => "declare i32 @socket(i32, i32, i32)\n",
            Self::NetSetSockOpt => "declare i32 @setsockopt(i32, i32, i32, ptr, i32)\n",
            Self::NetBind => "declare i32 @bind(i32, ptr, i32)\n",
            Self::NetListen => "declare i32 @listen(i32, i32)\n",
            Self::NetAccept => "declare i32 @accept(i32, ptr, ptr)\n",
            Self::NetRecv => "declare i64 @recv(i32, ptr, i64, i32)\n",
            Self::NetSend => "declare i64 @send(i32, ptr, i64, i32)\n",
            Self::NetGetFlags | Self::NetSetFlags => "declare i32 @fcntl(i32, i32, ...)\n",
            Self::NetConnect => "declare i32 @connect(i32, ptr, i32)\n",
            Self::NetResolve => "declare i32 @getaddrinfo(ptr, ptr, ptr, ptr)\n",
            Self::NetFreeAddresses => "declare void @freeaddrinfo(ptr)\n",
            Self::NetGetSockOpt => "declare i32 @getsockopt(i32, i32, i32, ptr, ptr)\n",
            Self::NetPoll => "declare i32 @poll(ptr, i64, i32)\n",
            Self::ReadFd => "declare i64 @read(i32, ptr, i64)\n",
            Self::GetEnvAddress => "declare ptr @getenv(ptr)\n",
            Self::BorrowText => "",
            Self::GetRandom => "declare i64 @getrandom(ptr, i64, i32)\n",
            Self::AtomicLoad | Self::AtomicStore | Self::AtomicSwap |
            Self::AtomicFetchAdd | Self::AtomicFetchSub | Self::AtomicFetchAnd |
            Self::AtomicFetchOr | Self::AtomicFetchXor | Self::AtomicCompareExchange => "",
            Self::ProcessOpen => "declare ptr @popen(ptr, ptr)\n",
            Self::ProcessReadLine => "declare ptr @fgets(ptr, i32, ptr)\n",
            Self::ProcessEof => "declare i32 @feof(ptr)\n",
            Self::ProcessClose => "declare i32 @pclose(ptr)\n",
        }
    }
    /// For a declared compiler intrinsic, synthesize its function definition.
    /// A named wrapper keeps function references valid, even at -O0. LLVM can
    /// inline/remove it at higher optimization levels or through GC sections.
    pub fn definition(self, symbol: &str) -> String {
        let (result, params) = self.signature();
        let parameters = params.iter().enumerate()
            .map(|(i, ty)| format!("{ty} %arg{i}"))
            .collect::<Vec<_>>().join(", ");
        let call = match self {
            Self::CopyBytes =>
                "call void @llvm.memcpy.p0.p0.i64(ptr align 1 %arg0, ptr align 1 %arg1, i64 %arg2, i1 false)\n  ret void".to_string(),
            Self::MoveBytes =>
                "call void @llvm.memmove.p0.p0.i64(ptr align 1 %arg0, ptr align 1 %arg1, i64 %arg2, i1 false)\n  ret void".to_string(),
            Self::SetBytes =>
                "call void @llvm.memset.p0.i64(ptr align 1 %arg0, i8 %arg1, i64 %arg2, i1 false)\n  ret void".to_string(),
            Self::Popcount64 => "call i64 @llvm.ctpop.i64(i64 %arg0)".into(),
            Self::LeadingZeros64 => "call i64 @llvm.ctlz.i64(i64 %arg0, i1 false)".into(),
            Self::TrailingZeros64 => "call i64 @llvm.cttz.i64(i64 %arg0, i1 false)".into(),
            Self::ByteSwap64 => "call i64 @llvm.bswap.i64(i64 %arg0)".into(),
            Self::ReverseBits64 => "call i64 @llvm.bitreverse.i64(i64 %arg0)".into(),
            Self::SqrtF64 => "call double @llvm.sqrt.f64(double %arg0)".into(),
            Self::AbsF64 => "call double @llvm.fabs.f64(double %arg0)".into(),
            Self::FloorF64 => "call double @llvm.floor.f64(double %arg0)".into(),
            Self::CeilF64 => "call double @llvm.ceil.f64(double %arg0)".into(),
            Self::HeapAllocate => "call ptr @aligned_alloc(i64 %arg0, i64 %arg1)".into(),
            Self::HeapFree => {
                return format!("define void @{symbol}({parameters}) {{\nentry:\n  %pointer = inttoptr i64 %arg0 to ptr\n  call void @free(ptr %pointer)\n  ret void\n}}\n\n");
            },
            Self::TextLength => {
                // strlen loop with no dependency on libc strlen. Input str is
                // borrowed NUL-terminated UTF-8; its owner must remain alive.
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  br label %test\ntest:\n  %index = phi i64 [0, %entry], [%next, %step]\n  %address = getelementptr i8, ptr %arg0, i64 %index\n  %byte = load i8, ptr %address, align 1\n  %zero = icmp eq i8 %byte, 0\n  br i1 %zero, label %finished, label %step\nstep:\n  %next = add i64 %index, 1\n  br label %test\nfinished:\n  ret i64 %index\n}}\n\n");
            },
            Self::TextAddress => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %address = ptrtoint ptr %arg0 to i64\n  ret i64 %address\n}}\n\n");
            },
            Self::WriteBytes => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %ptr = inttoptr i64 %arg1 to ptr\n  %value = call i64 @write(i32 %arg0, ptr %ptr, i64 %arg2)\n  ret i64 %value\n}}\n\n");
            },
            Self::ProcessExit => {
                return format!("define void @{symbol}({parameters}) noreturn {{\nentry:\n  call void @_Exit(i32 %arg0)\n  unreachable\n}}\n\n");
            },
            Self::LoadByte => {
                return format!("define i8 @{symbol}({parameters}) {{\nentry:\n  %ptr = inttoptr i64 %arg0 to ptr\n  %byte = load i8, ptr %ptr, align 1\n  ret i8 %byte\n}}\n\n");
            },
            Self::OpenAt => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %result = call i32 @openat(i32 %arg0, ptr %arg1, i32 %arg2, i32 %arg3)\n  ret i32 %result\n}}\n\n");
            },
            Self::CloseFd => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %result = call i32 @close(i32 %arg0)\n  ret i32 %result\n}}\n\n");
            },
            Self::ClockGetTime => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %result = call i32 @clock_gettime(i32 %arg0, ptr %arg1)\n  ret i32 %result\n}}\n\n");
            },
            Self::Errno => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %address = call ptr @__errno_location()\n  %value = load i32, ptr %address, align 4\n  ret i32 %value\n}}\n\n");
            },
            Self::ThreadStart => {
                // POSIX pthread_t is an unsigned machine word on Linux 64-bit.
                // The application callback must have the Nova-checked fn()->() ABI.
                // The thunk translates it to pthread's ptr(ptr) callback ABI.
                return format!("define internal ptr @__nova_llvm_thread_entry(ptr %callback) {{\nentry:\n  call void %callback()\n  ret ptr null\n}}\n\ndefine i32 @{symbol}({parameters}) {{\nentry:\n  %err = call i32 @pthread_create(ptr %arg1, ptr null, ptr @__nova_llvm_thread_entry, ptr %arg0)\n  ret i32 %err\n}}\n\n");
            },
            Self::ThreadStartArg => "call i32 @pthread_create(ptr %arg2, ptr null, ptr %arg0, ptr %arg1)".into(),
            Self::ThreadJoin => "call i32 @pthread_join(i64 %arg0, ptr null)".into(),
            Self::ThreadYield => "call i32 @sched_yield()".into(),
            Self::SleepMillis => {
                // Linux x86-64/aarch64 timespec: seconds and nanoseconds are i64.
                // No Nova-owned C runtime. The higher-level STD handles EINTR.
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %seconds = udiv i64 %arg0, 1000\n  %rest = urem i64 %arg0, 1000\n  %nanos = mul i64 %rest, 1000000\n  %timespec = alloca {{ i64, i64 }}, align 8\n  %seconds_ptr = getelementptr {{ i64, i64 }}, ptr %timespec, i32 0, i32 0\n  %nanos_ptr = getelementptr {{ i64, i64 }}, ptr %timespec, i32 0, i32 1\n  store i64 %seconds, ptr %seconds_ptr, align 8\n  store i64 %nanos, ptr %nanos_ptr, align 8\n  %status = call i32 @nanosleep(ptr %timespec, ptr null)\n  ret i32 %status\n}}\n\n");
            },
            Self::ThreadCurrent => "call i64 @pthread_self()".into(),
            Self::WinStdHandle => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %handle = call ptr @GetStdHandle(i32 %arg0)\n  %address = ptrtoint ptr %handle to i64\n  ret i64 %address\n}}\n\n");
            },
            Self::WinWriteFile => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %handle = inttoptr i64 %arg0 to ptr\n  %data = inttoptr i64 %arg1 to ptr\n  %result = call i32 @WriteFile(ptr %handle, ptr %data, i32 %arg2, ptr %arg3, ptr null)\n  ret i32 %result\n}}\n\n");
            },
            Self::WinExitProcess => {
                return format!("define void @{symbol}({parameters}) noreturn {{\nentry:\n  call void @ExitProcess(i32 %arg0)\n  unreachable\n}}\n\n");
            },
            Self::NetSocket => "call i32 @socket(i32 %arg0, i32 %arg1, i32 %arg2)".into(),
            Self::NetSetSockOpt => "call i32 @setsockopt(i32 %arg0, i32 %arg1, i32 %arg2, ptr %arg3, i32 %arg4)".into(),
            Self::NetBind => "call i32 @bind(i32 %arg0, ptr %arg1, i32 %arg2)".into(),
            Self::NetListen => "call i32 @listen(i32 %arg0, i32 %arg1)".into(),
            Self::NetAccept => "call i32 @accept(i32 %arg0, ptr null, ptr null)".into(),
            Self::NetRecv => "call i64 @recv(i32 %arg0, ptr %arg1, i64 %arg2, i32 %arg3)".into(),
            Self::NetSend => "call i64 @send(i32 %arg0, ptr %arg1, i64 %arg2, i32 %arg3)".into(),
            Self::NetGetFlags => "call i32 (i32, i32, ...) @fcntl(i32 %arg0, i32 %arg1)".into(),
            Self::NetSetFlags => "call i32 (i32, i32, ...) @fcntl(i32 %arg0, i32 %arg1, i32 %arg2)".into(),
            Self::AtomicLoad | Self::AtomicStore | Self::AtomicSwap |
            Self::AtomicFetchAdd | Self::AtomicFetchSub | Self::AtomicFetchAnd |
            Self::AtomicFetchOr | Self::AtomicFetchXor | Self::AtomicCompareExchange => {
                let symbol_name = format!("std__abi__llvm__{}", self.label());
                let template = include_str!("llvm/atomic_i64.ll");
                let marker = format!("; NOVA_ATOMIC {symbol_name}\n");
                let remainder = template.split_once(&marker)
                    .unwrap_or_else(|| panic!("missing atomic LLVM template: {symbol_name}"))
                    .1;
                let definition = remainder.split("; NOVA_ATOMIC ").next().unwrap();
                return definition.replace(&symbol_name, symbol);
            },
            Self::NetConnect => "call i32 @connect(i32 %arg0, ptr %arg1, i32 %arg2)".into(),
            // The public Nova binding keeps hints opaque; the compiler owns
            // the null-hints ABI required for the first portable resolver.
            Self::NetResolve => "call i32 @getaddrinfo(ptr %arg0, ptr %arg1, ptr null, ptr %arg3)".into(),
            Self::NetFreeAddresses => "call void @freeaddrinfo(ptr %arg0)\n  ret void".into(),
            Self::NetGetSockOpt => "call i32 @getsockopt(i32 %arg0, i32 %arg1, i32 %arg2, ptr %arg3, ptr %arg4)".into(),
            Self::NetPoll => "call i32 @poll(ptr %arg0, i64 %arg1, i32 %arg2)".into(),
            Self::ReadFd => "call i64 @read(i32 %arg0, ptr %arg1, i64 %arg2)".into(),
            Self::GetRandom => "call i64 @getrandom(ptr %arg0, i64 %arg1, i32 %arg2)".into(),
            Self::GetEnvAddress => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %address = call ptr @getenv(ptr %arg0)\n  %value = ptrtoint ptr %address to i64\n  ret i64 %value\n}}\n\n");
            },
            Self::BorrowText => {
                return format!("define ptr @{symbol}({parameters}) {{\nentry:\n  %address = inttoptr i64 %arg0 to ptr\n  ret ptr %address\n}}\n\n");
            },
            Self::PointerOffset => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %base = inttoptr i64 %arg0 to ptr\n  %result = getelementptr i8, ptr %base, i64 %arg1\n  %address = ptrtoint ptr %result to i64\n  ret i64 %address\n}}\n\n");
            },
            Self::ProcessOpen => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %stream = call ptr @popen(ptr %arg0, ptr %arg1)\n  %address = ptrtoint ptr %stream to i64\n  ret i64 %address\n}}\n\n");
            },
            Self::ProcessReadLine => {
                return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %destination = inttoptr i64 %arg0 to ptr\n  %stream = inttoptr i64 %arg2 to ptr\n  %line = call ptr @fgets(ptr %destination, i32 %arg1, ptr %stream)\n  %address = ptrtoint ptr %line to i64\n  ret i64 %address\n}}\n\n");
            },
            Self::ProcessEof => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %stream = inttoptr i64 %arg0 to ptr\n  %status = call i32 @feof(ptr %stream)\n  ret i32 %status\n}}\n\n");
            },
            Self::ProcessClose => {
                return format!("define i32 @{symbol}({parameters}) {{\nentry:\n  %stream = inttoptr i64 %arg0 to ptr\n  %status = call i32 @pclose(ptr %stream)\n  ret i32 %status\n}}\n\n");
            },
        };
        if self == Self::HeapAllocate {
            return format!("define i64 @{symbol}({parameters}) {{\nentry:\n  %pointer = {call}\n  %address = ptrtoint ptr %pointer to i64\n  ret i64 %address\n}}\n\n");
        }
        if result == "void" {
            format!("define void @{symbol}({parameters}) {{\nentry:\n  {call}\n}}\n\n")
        } else {
            format!("define {result} @{symbol}({parameters}) {{\nentry:\n  %value = {call}\n  ret {result} %value\n}}\n\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Intrinsic;
    #[test]
    fn llvm_pthread_bridge_does_not_depend_on_custom_c_shims() {
        let typed = Intrinsic::ThreadStartArg;
        assert_eq!(typed.signature(), ("i32", &["ptr", "ptr", "ptr"][..]));
        assert!(typed.is_unsafe());
        assert!(typed.definition("std__abi__llvm__thread_start_arg").contains("call i32 @pthread_create"));
        assert!(Intrinsic::ThreadStart.definition("std__abi__llvm__thread_start")
            .contains("define internal ptr @__nova_llvm_thread_entry"));
        assert!(Intrinsic::ThreadJoin.definition("std__abi__llvm__thread_join")
            .contains("call i32 @pthread_join"));
    }
    #[test]
    fn nanosleep_wrapper_uses_posix_timespec_without_custom_runtime() {
        let sleep = Intrinsic::SleepMillis;
        assert_eq!(sleep.signature(), ("i32", &["i64"][..]));
        assert_eq!(sleep.declaration(), "declare i32 @nanosleep(ptr, ptr)\n");
        let ir = sleep.definition("std__abi__llvm__sleep_millis");
        assert!(ir.contains("udiv i64 %arg0, 1000"));
        assert!(ir.contains("call i32 @nanosleep(ptr %timespec, ptr null)"));
        assert!(!ir.contains("__nova_coro"));
    }
    #[test]
    fn llvm_intrinsic_abi_is_closed_and_unique() {
        let mut labels = std::collections::HashSet::new();
        for item in Intrinsic::ALL {
            assert!(labels.insert(item.label()));
            let (_, params) = item.signature();
            assert!(!params.is_empty() || matches!(item, Intrinsic::Errno | Intrinsic::ThreadYield | Intrinsic::ThreadCurrent));
            assert!(item.definition("unit_test_intrinsic").contains("define "));
        }
        assert!(Intrinsic::from_name("user__abi__llvm__copy_bytes").is_none());
    }
}

#[cfg(test)]
mod windows_abi_tests {
    use super::Intrinsic;
    #[test]
    fn windows_x64_uses_win32_not_posix_calls() {
        let handle = Intrinsic::WinStdHandle.definition("std__abi__llvm__win_std_handle");
        let write = Intrinsic::WinWriteFile.definition("std__abi__llvm__win_write_file");
        let exit = Intrinsic::WinExitProcess.definition("std__abi__llvm__win_exit_process");
        assert!(handle.contains("@GetStdHandle(i32 %arg0)"));
        assert!(write.contains("@WriteFile(ptr %handle"));
        assert!(exit.contains("@ExitProcess(i32 %arg0)"));
        assert!(!write.contains("@write("), "Windows must not inherit Linux write(2)");
    }
}
