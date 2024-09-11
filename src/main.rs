#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::{
    clock::ClockControl,
    cpu_control::{CpuControl, Stack},
    gpio::{AnyOutput, Io, Level},
    interrupt::Priority,
    peripherals::Peripherals,
    prelude::*,
    system::SystemControl,
    timer::timg::TimerGroup,
};
use esp_hal_embassy::InterruptExecutor;
use sign_firmware::{leds_software_pwm, Leds};
use static_cell::StaticCell;

extern crate alloc;
use core::{mem::MaybeUninit, ptr::addr_of_mut};

#[global_allocator]
static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();

fn init_heap() {
    const HEAP_SIZE: usize = 32 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

    unsafe {
        ALLOCATOR.init(HEAP.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}

static mut APP_CORE_STACK: Stack<8192> = Stack::new();

#[entry]
fn main() -> ! {
    init_heap();

    let peripherals = Peripherals::take();

    let io = Io::new(peripherals.GPIO, peripherals.IO_MUX);

    let system = SystemControl::new(peripherals.SYSTEM);
    let sw_ints = system.software_interrupt_control;

    let clocks = ClockControl::max(system.clock_control).freeze();
    let timg0 = TimerGroup::new(peripherals.TIMG1, &clocks);
    esp_hal_embassy::init(&clocks, timg0.timer0);

    // let _init = esp_wifi::initialize(
    //     esp_wifi::EspWifiInitFor::Wifi,
    //     timg0.timer1,
    //     esp_hal::rng::Rng::new(peripherals.RNG),
    //     peripherals.RADIO_CLK,
    //     &clocks,
    // )
    // .unwrap();

    let mut cpu_control = CpuControl::new(peripherals.CPU_CTRL);

    let leds = [
        AnyOutput::new(io.pins.gpio1, Level::Low),
        AnyOutput::new(io.pins.gpio2, Level::Low),
        AnyOutput::new(io.pins.gpio4, Level::Low),
        AnyOutput::new(io.pins.gpio5, Level::Low),
        AnyOutput::new(io.pins.gpio6, Level::Low),
        AnyOutput::new(io.pins.gpio7, Level::Low),
        AnyOutput::new(io.pins.gpio8, Level::Low),
        AnyOutput::new(io.pins.gpio9, Level::Low),
        AnyOutput::new(io.pins.gpio10, Level::Low),
        AnyOutput::new(io.pins.gpio11, Level::Low),
        AnyOutput::new(io.pins.gpio12, Level::Low),
        AnyOutput::new(io.pins.gpio13, Level::Low),
        AnyOutput::new(io.pins.gpio14, Level::Low),
        AnyOutput::new(io.pins.gpio15, Level::Low),
        AnyOutput::new(io.pins.gpio16, Level::Low),
    ];

    static EXECUTOR_CORE_1: StaticCell<InterruptExecutor<1>> = StaticCell::new();
    let executor_core1 = InterruptExecutor::new(sw_ints.software_interrupt1);
    let executor_core1 = EXECUTOR_CORE_1.init(executor_core1);

    let _guard = cpu_control
        .start_app_core(unsafe { &mut *addr_of_mut!(APP_CORE_STACK) }, move || {
            let spawner = executor_core1.start(Priority::Priority1);

            spawner.spawn(leds_software_pwm(leds)).ok();

            // Just loop to show that the main thread does not need to poll the executor.
            loop {}
        })
        .unwrap();

    static EXECUTOR_CORE_0: StaticCell<InterruptExecutor<0>> = StaticCell::new();
    let executor_core0 = InterruptExecutor::new(sw_ints.software_interrupt0);
    let executor_core0 = EXECUTOR_CORE_0.init(executor_core0);

    let spawner = executor_core0.start(Priority::Priority1);

    let leds = Leds::create();

    // Just loop to show that the main thread does not need to poll the executor.
    loop {}
}
