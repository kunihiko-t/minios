//! QEMU `virt`のPLIC (Platform-Level Interrupt Controller) へのMMIO driver。
//!
//! 割り込みsourceをS-mode contextへ配送する最小限の経路だけを扱う。
//! claim/completeで受け付けたsourceはdeviceごとの処置へ振り分け、割り込みで
//! 到着したbyteの消費自体は従来どおりconsumer (stdin staging) のpoll経路へ
//! 任せる。PLICが無いmachine (`MachineSpec::plic_base == 0`) ではこのdriverは
//! 初期化されず、全経路がpollingのまま動く。

use core::sync::atomic::{AtomicUsize, Ordering};

/// QEMU `virt`のUART0が接続されるPLIC source番号。
pub const UART_IRQ: u32 = 10;

/// QEMU `virt`ではcontext 0がM-mode hart 0、context 1がS-mode hart 0である。
const S_MODE_CONTEXT: usize = 1;

/// sourceごとの優先度register。`priority[source]`は`base + 4*source`にある。
const PRIORITY_BASE: usize = 0x0000;
/// contextごとのenableビットマップ領域。context strideは0x80 byte。
const ENABLE_BASE: usize = 0x2000;
const ENABLE_CONTEXT_STRIDE: usize = 0x80;
/// contextごとのthreshold/claim block。context strideは0x1000 byteで、
/// block先頭がthreshold、+4がclaim/complete registerである。
const THRESHOLD_BASE: usize = 0x20_0000;
const CONTEXT_BLOCK_STRIDE: usize = 0x1000;
const CLAIM_OFFSET: usize = 0x4;

// Safety: `init`が一度だけ書き、その後は不変。0のときdriverは無効であり、
// claim/completeへのアクセスは行われない。
static PLIC_BASE: AtomicUsize = AtomicUsize::new(0);
static UART_IRQ_CLAIMS: AtomicUsize = AtomicUsize::new(0);

fn plic() -> *mut u8 {
    PLIC_BASE.load(Ordering::Relaxed) as *mut u8
}

/// PLICのベースを登録し、S-mode contextのthresholdを0へ下げる。
/// `kernel_main`が割り込み経路を立てるとき一度だけ呼ぶ。
pub fn init(base: usize) {
    PLIC_BASE.store(base, Ordering::Relaxed);
    // Safety: `base`はFDTが報告するPLICのMMIO pageであり、threshold registerは
    // context blockの先頭にある。volatile writeで優先度0以上を全て通す。
    unsafe {
        core::ptr::write_volatile(
            plic().add(THRESHOLD_BASE + S_MODE_CONTEXT * CONTEXT_BLOCK_STRIDE) as *mut u32,
            0,
        );
    }
}

/// sourceをS-mode contextへ配送するよう優先度とenable bitを立てる。
pub fn enable(source: u32) {
    // Safety: `plic()`は`init`済みの有効なPLIC MMIOを指し、優先度とenable
    // bitmapは規定のoffsetにある。bit単位のread-modify-writeは他sourceの
    // enable状態を保つ。
    unsafe {
        core::ptr::write_volatile(
            plic().add(PRIORITY_BASE + source as usize * 4) as *mut u32,
            1,
        );
        let enable = plic().add(ENABLE_BASE + S_MODE_CONTEXT * ENABLE_CONTEXT_STRIDE) as *mut u32;
        let word = source / 32;
        let bit = source % 32;
        let current = core::ptr::read_volatile(enable.add(word as usize));
        core::ptr::write_volatile(enable.add(word as usize), current | (1 << bit));
    }
}

fn claim() -> u32 {
    // Safety: claim registerはcontext blockの先頭+4にあり、読むと最優先の
    // pending sourceを返してpendingを下げる。0は「pendingなし」を意味する。
    unsafe {
        core::ptr::read_volatile(
            plic().add(THRESHOLD_BASE + S_MODE_CONTEXT * CONTEXT_BLOCK_STRIDE + CLAIM_OFFSET)
                as *const u32,
        )
    }
}

fn complete(source: u32) {
    // Safety: claim registerへsource番号を書き戻すと配送が完了する。
    unsafe {
        core::ptr::write_volatile(
            plic().add(THRESHOLD_BASE + S_MODE_CONTEXT * CONTEXT_BLOCK_STRIDE + CLAIM_OFFSET)
                as *mut u32,
            source,
        );
    }
}

/// pendingなexternal割り込みをclaimし、sourceごとの処置を行ってcompleteする。
/// trap handler (SIE=1経路) と、S-mode割り込みを遮断したまま`wfi`/pollする
/// 待機窓 (SEIP pending経路) の両方が呼ぶ。PLIC未初期化のmachineでは何もしない。
pub fn handle_external_pending() {
    if PLIC_BASE.load(Ordering::Relaxed) == 0 {
        return;
    }
    loop {
        let source = claim();
        if source == 0 {
            return;
        }
        if source == UART_IRQ {
            UART_IRQ_CLAIMS.fetch_add(1, Ordering::Relaxed);
            // 16550のRX-ready割り込みはFIFOにbyteが残る限りlevelで再発火
            // し続ける。byteの消費はconsumerのpollへ任せるため、drainが
            // 終わるまでdevice側の割り込みをmaskして暴走を止める。
            crate::drivers::uart::Uart::for_target().mask_rx_interrupt();
        }
        complete(source);
    }
}

/// これまでにclaimしたUART RX割り込みの回数。`irq-uart`検査が、
/// stdin待ちprocessの起床がpollingではなく割り込み経由だったことを観測する。
#[cfg(feature = "qemu-test-irq-uart")]
pub fn uart_irq_claims() -> usize {
    UART_IRQ_CLAIMS.load(Ordering::Relaxed)
}
