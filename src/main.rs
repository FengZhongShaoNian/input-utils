use evdev::{Device, EventStream, EventSummary, InputEvent, KeyCode, KeyEvent, RelativeAxisCode, RelativeAxisEvent};
use mouse_keyboard_input::VirtualDevice;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::{future, io};
use strum::{EnumIter};
use tokio::select;

#[derive(Debug, Deserialize)]
struct Config {
    devices: Devices,
    rules: Vec<Rule>
}

#[derive(Debug, Deserialize)]
struct Devices {
    keyboard: String,
    mouse: String,
}
#[derive(Debug, Deserialize, Clone)]
struct Rule {
    /// 某个键
    key: Key,
    /// 对这个键的什么事件感兴趣
    rule_type: RuleType,
    /// 当感兴趣的事件发生时需要执行的操作
    action: Action
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, EnumIter, Hash)]
enum Key {
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftShift,
    RightShift,
    LeftMeta, // 左侧的Win键
    RightMeta,// 右侧的Win键
    BtnSide, // 鼠标的两个侧键中靠后的那个键
    BtnExtra, // 鼠标的两个侧键中靠前的那个键
    BtnRight // 鼠标右键
}

impl Into<KeyCode> for Key{
    fn into(self) -> KeyCode {
        match self {
            Key::LeftCtrl => KeyCode::KEY_LEFTCTRL,
            Key::RightCtrl => KeyCode::KEY_RIGHTCTRL,
            Key::LeftAlt => KeyCode::KEY_LEFTALT,
            Key::RightAlt => KeyCode::KEY_RIGHTALT,
            Key::LeftShift => KeyCode::KEY_LEFTSHIFT,
            Key::RightShift => KeyCode::KEY_RIGHTSHIFT,
            Key::LeftMeta => KeyCode::KEY_LEFTMETA,
            Key::RightMeta => KeyCode::KEY_RIGHTMETA,
            Key::BtnSide => KeyCode::BTN_SIDE,
            Key::BtnExtra => KeyCode::BTN_EXTRA,
            Key::BtnRight => KeyCode::BTN_RIGHT
        }
    }
}

impl From<KeyCode> for Key {
    fn from(value: KeyCode) -> Self {
        match value {
            KeyCode::KEY_LEFTCTRL =>  Key::LeftCtrl,
            KeyCode::KEY_RIGHTCTRL => Key::RightCtrl,
            KeyCode::KEY_LEFTALT => Key::LeftAlt,
            KeyCode::KEY_RIGHTALT => Key::RightAlt,
            KeyCode::KEY_LEFTSHIFT => Key::LeftShift,
            KeyCode::KEY_RIGHTSHIFT => Key::RightShift,
            KeyCode::KEY_LEFTMETA => Key::LeftMeta,
            KeyCode::KEY_RIGHTMETA => Key::RightMeta,
            KeyCode::BTN_SIDE => Key::BtnSide,
            KeyCode::BTN_EXTRA => Key::BtnExtra,
            KeyCode::BTN_RIGHT => Key::BtnRight,
            _ => panic!("Unsupported key {:?} for input", value)
        }
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>
    {
        let key_str = String::deserialize(deserializer)?;
        key_str.trim().parse::<KeyCode>().map_err(serde::de::Error::custom).map(Key::from)
    }
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq)]
enum RuleType {
    /// 对某个键的【单击】事件感兴趣
    Click,

    /// 对某个键的【双击】事件感兴趣
    DoubleClick,

    /// 对【按下某个键并滚动鼠标滚轮】事件感兴趣
    ScrollWheelWithKeyPressed
}

#[derive(Debug, Clone, Serialize)]
struct ShortcutString(String);

impl ShortcutString {
    pub fn new(s: String) -> Result<Self, String> {
        Self::get_key_codes(s.as_str())?;
        Ok(Self(s))
    }

    pub fn get_key_codes(shortcut: &str) -> Result<Vec<KeyCode>, String> {
        let keys: Vec<&str> = shortcut.split('+').collect();
        let mut key_codes: Vec<KeyCode> = vec![];
        for key in keys {
            match key.trim().parse::<KeyCode>() {
                Ok(key_code) => {
                    key_codes.push(key_code);
                }
                Err(_) => {
                    return Err(format!("Unable to parse key: {}", key));
                }
            }
        }
        Ok(key_codes)
    }
}

impl<'de> Deserialize<'de> for ShortcutString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>
    {
        String::deserialize(deserializer)
            .and_then(|s| ShortcutString::new(s).map_err(serde::de::Error::custom))
    }
}

#[derive(Debug, Clone, Serialize)]
struct HoldingKey(String);
impl HoldingKey {
    pub fn new(s: String) -> Result<Self, String> {
        Self::get_key_codes(s.as_str())?;
        Ok(Self(s))
    }

    pub fn get_key_codes(s: &str) -> Result<KeyCode, String> {
        let key_code = s.trim().parse::<KeyCode>().map_err(|_|format!("Unable to parse key: {}", s))?;
        Ok(key_code)
    }
}

impl<'de> Deserialize<'de> for HoldingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>
    {
        String::deserialize(deserializer)
            .and_then(|s| HoldingKey::new(s).map_err(serde::de::Error::custom))
    }
}

/// 当指定的事件发生的时候需要执行的操作
#[derive(Clone, Debug, Deserialize, Serialize)]
enum Action {
    /// 参数是快捷键（例如："KEY_LEFTCTRL+KEY_C"）
    Shortcut(ShortcutString),
    /// 第一个参数上快捷键（例如："KEY_LEFTALT+KEY_TAB"），第二个参数是需要持续按住的键（例如："KEY_LEFTALT"）
    /// 仅ScrollWheelWithKeyPressed类型的规则可以使用这种类型的快捷键
    ShortcutWithKeyHolding(ShortcutString, HoldingKey)
}

#[derive(Copy, Clone, Debug)]
enum State {
    /// 初始状态
    Init,

    /// 只有对某个键的【单击】、【双击】、【按下某个键并滚动鼠标滚轮】这3个事件的任一事件感兴趣，才会流转到这个状态。
    /// 表示当前处于【某个键首次被按下】的状态。
    /// 可能发生的行为有：键持续按下并滚动鼠标滚轮
    /// 1. 处于 KeyDownFirstTime 状态，忽略鼠标移动事件（直接透传鼠标移动事件），下一个事件是滚轮事件，那么【键持续按下并滚动鼠标滚轮】行为发生了，不流转状态。
    /// 2. 处于 KeyDownFirstTime 状态，如果下一个事件是其它键的按下事件，那么透传，同时状态流转回Init状态。
    /// 3. 处于 KeyDownFirstTime 状态，如果下一个事件是当前键松开的事件，那么转为KeyUpFirstTime状态。
    KeyDownFirstTime(KeyEvent),

    /// 只有对某个键的【单击】、【双击】这2个事件的任一事件感兴趣，才会流转到这个状态。
    /// 某个键被首次松开。可能发生的行为有：单击、双击
    /// 1.处于 KeyUpFirstTime 状态，若对当前键的单击事件不感兴趣，那么判断是否需要关注它的双击事件，若只需要关注双击事件，那么不流转状态，若都不需要关注，那么透传并流转回Init状态
    /// 2.处于 KeyUpFirstTime 状态，200毫秒内相同的键没有被再次按下（250毫秒内没有任何键按下、下一个事件上其它键的按下事件），那么【单击】行为发生了，流转回Init状态。
    /// 3.处于 KeyUpFirstTime 状态，200毫秒内相同的键被再次按下了，那么【双击】行为发生了，流转回Init状态。
    KeyUpFirstTime(KeyEvent),
}

impl State {
    pub(crate) async fn timeout(&self) {
        match self {
            State::Init => {
                let future = future::pending();
                let () = future.await;
            }
            State::KeyDownFirstTime(_) => {
                let future = future::pending();
                let () = future.await;
            }
            State::KeyUpFirstTime(_) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum KeyAction {
    Pressed,
    Holding,
    Released
}

impl From<i32> for KeyAction {
    fn from(value: i32) -> Self {
        match value {
            0 => KeyAction::Released,
            1 => KeyAction::Pressed,
            2 => KeyAction::Holding,
            _ => panic!("Unknown key action for value: {}", value)
        }
    }
}

fn send_key_event(virtual_input_device: Arc<Mutex<VirtualDevice>>, key_action: KeyAction, key_code: KeyCode) {
    let mut device = virtual_input_device.lock().unwrap();
    if key_action == KeyAction::Pressed {
        device.press(key_code.0).unwrap();
    } else if key_action == KeyAction::Released {
        device.release(key_code.0).unwrap();
    }
}

fn send_mouse_event(virtual_input_device: Arc<Mutex<VirtualDevice>>, event: RelativeAxisEvent) {
    let axis_code = event.code();
    let mut device = virtual_input_device.lock().unwrap();
    if axis_code == RelativeAxisCode::REL_X {
        device.move_mouse_x(event.value()).unwrap();
    }else if axis_code == RelativeAxisCode::REL_Y {
        device.move_mouse_y(-event.value()).unwrap();
    }else if axis_code == RelativeAxisCode::REL_WHEEL {
        device.scroll_y(event.value()).unwrap();
    } else if axis_code == RelativeAxisCode::REL_WHEEL_HI_RES {
        device.scroll_y(event.value()).unwrap();
    }
}

struct StateMachine {
    state: State,
    config: Config,

    /// 感兴趣的KeyCode
    interest_keys: Vec<KeyCode>,

    /// 与某个Key对应的规则类型
    key_rule_types: HashMap<KeyCode, Vec<RuleType>>,

    /// 与某个Key对应的规则
    key_rules: HashMap<KeyCode, Vec<Rule>>,

    virtual_input_device: Arc<Mutex<VirtualDevice>>,

    shortcut_with_key_holding: Option<Action>,
}

impl StateMachine {
    fn new(config: Config, virtual_input_device: Arc<Mutex<VirtualDevice>>) -> Self {
        let rules = &config.rules;
        let interest_keys = rules.iter().map(|rule| rule.key.into()).collect();
        let mut key_rule_types: HashMap<KeyCode, Vec<RuleType>> = HashMap::new();
        let mut key_rules: HashMap<KeyCode, Vec<Rule>> = HashMap::new();
        for rule in rules {
            let types = key_rule_types.get_mut(&rule.key.into());
             if let Some(types) = types {
                 types.push(rule.rule_type.into());
             }else {
                 let types = vec![rule.rule_type.into()];
                 key_rule_types.insert(rule.key.into(), types);
             }

            let rules_for_key = key_rules.get_mut(&rule.key.into());
            if let Some(rules_for_key) = rules_for_key {
                rules_for_key.push(rule.clone());
            }else {
                let rules_for_key = vec![rule.clone()];
                key_rules.insert(rule.key.into(), rules_for_key);
            }
        }
        Self {
            state: State::Init,
            config,
            interest_keys,
            key_rule_types,
            key_rules,
            virtual_input_device,
            shortcut_with_key_holding: None,
        }
    }

    fn send_key_event(&mut self, key_action: KeyAction, key_code: KeyCode) {
        send_key_event(self.virtual_input_device.clone(), key_action, key_code);
    }

    fn send_mouse_event(&mut self, event: RelativeAxisEvent) {
        send_mouse_event(self.virtual_input_device.clone(), event);
    }

    fn get_action_for(&self, key_code: &KeyCode, rule_type: &RuleType) -> Option<Action> {
        let rules: &Vec<Rule> = self.key_rules.get(key_code)?;
        for rule in rules {
            if &rule.rule_type == rule_type {
                return Some(rule.action.clone());
            }
        }
        None
    }

    fn perform_shortcut_action(&mut self, action: &Action) {
        match action {
            Action::Shortcut(ShortcutString(s)) => {
                println!("Perform shortcut action: {}", s);
                let key_codes = ShortcutString::get_key_codes(&s).unwrap();
                for key_code in &key_codes {
                    self.send_key_event(KeyAction::Pressed, *key_code);
                }
                for key_code in &key_codes {
                    self.send_key_event(KeyAction::Released, *key_code);
                }
            }
            Action::ShortcutWithKeyHolding(ShortcutString(s), HoldingKey(h)) => {
                println!("Perform shortcut action: {}+{}, holding key is: {}", h, s, h);
                let key_codes = ShortcutString::get_key_codes(&s).unwrap();
                let holding_key = HoldingKey::get_key_codes(&h).unwrap();
                if self.shortcut_with_key_holding.is_none() {
                    self.send_key_event(KeyAction::Pressed, holding_key);
                    self.shortcut_with_key_holding = Some(action.clone());
                }
                for key in &key_codes {
                    self.send_key_event(KeyAction::Pressed, *key);
                }
                for key in &key_codes {
                    self.send_key_event(KeyAction::Released, *key);
                }
            }
        }
    }

    fn is_holding_key(&self, ) -> bool {
        if let Some(Action::ShortcutWithKeyHolding(ShortcutString(_), HoldingKey(_))) = &self.shortcut_with_key_holding {
            true
        }else {
            false
        }
    }

    fn release_holding_key(&mut self) {
        if let Some(Action::ShortcutWithKeyHolding(ShortcutString(_), HoldingKey(h))) = &self.shortcut_with_key_holding {
            println!("Releasing holding key: {}", h);
            let holding_key = HoldingKey::get_key_codes(&h).unwrap();
            self.send_key_event(KeyAction::Released, holding_key);
            self.shortcut_with_key_holding = None;
        }
    }

    /// 判断key_code是否对指定的rule_type感兴趣，感兴趣则返回true，否则返回false
    fn judge_interest(&self, key_code: KeyCode, rule_type: RuleType) -> bool {
        if self.key_rule_types.contains_key(&key_code) {
            if let Some(rule_types) = self.key_rule_types.get(&key_code) {
                rule_types.contains(&rule_type)
            }else {
                false
            }
        }else {
            false
        }
    }

    fn update_state(&mut self, new_state: State) {
        println!("Updating state from {:?} to {:?}", self.state, new_state);
        self.state = new_state;
    }
    
    fn accept(&mut self, event: InputEvent) {
        let rules = &self.config.rules;
        if rules.is_empty() {
            println!("No rules");
            return;
        }
        match event.destructure() {
            EventSummary::Key(key_event, key_code, value) => {
                let key_action: KeyAction = value.into();
                println!("当前状态：{:?}, Key: {:?}, _value: {}, action: {:?}", self.state, key_event, value, key_action);

                match self.state {
                    State::Init => {
                        if key_action == KeyAction::Pressed && self.interest_keys.contains(&key_code) {
                            self.update_state(State::KeyDownFirstTime(key_event));
                        }else {
                            println!("透传{:?}事件", key_event);
                            self.send_key_event(key_action, key_code);
                        }
                    }
                    State::KeyDownFirstTime(previous_key_down_event) => {
                        // 当前状态是 KeyDownFirstTime 状态，意味着至少对之前按下的这个键的【单击】、【双击】、【按下并滚动鼠标滚轮】这3个事件之一感兴趣

                        if key_action == KeyAction::Pressed && previous_key_down_event.code() != key_code {
                            // 处于 KeyDownFirstTime 状态，如果下一个事件是其它键的按下事件，那么透传，同时状态流转回Init状态
                            self.send_key_event(KeyAction::Pressed, previous_key_down_event.code());
                            self.send_key_event(KeyAction::Pressed, key_event.code());
                            self.update_state(State::Init);
                        }else if key_action == KeyAction::Released && previous_key_down_event.code() == key_code {
                            if self.is_holding_key() {
                                // 进入这里说明 ScrollWheelWithKeyPressed 事件已经发生
                                self.release_holding_key();
                                self.update_state(State::Init);
                                return;
                            }
                            // 处于 KeyDownFirstTime 状态，进入这里说明 ScrollWheelWithKeyPressed 事件没有发生，
                            // 判断是否对当前键的单击或者双击事件感兴趣，
                            // 如果感兴趣，那么流转为 KeyUpFirstTime 状态
                            // 如果都不感兴趣，而当前又能走到 KeyDownFirstTime 状态，说明只对当前键的 ScrollWheelWithKeyPressed 事件感兴趣，那么流转回Init状态
                            if self.judge_interest(previous_key_down_event.code(), RuleType::Click) ||
                                self.judge_interest(previous_key_down_event.code(), RuleType::DoubleClick) {
                                self.update_state(State::KeyUpFirstTime(key_event));
                            }else {
                                // 只对当前键的 ScrollWheelWithKeyPressed 事件感兴趣
                                self.update_state(State::Init);
                            }
                        }
                    }
                    State::KeyUpFirstTime(previous_key_up_event) => {
                        // 当前处于 KeyDownFirstTime ，说明至少对当前键的【单击】或者【双击】事件感兴趣

                        // 处于 KeyUpFirstTime 状态，如果下一个事件上其它键的按下事件，这意味着【单击】事件发生了，而且【双击】事件不可能发生
                        if key_action == KeyAction::Pressed && previous_key_up_event.code() != key_code {
                            // 判断是否对当前键的【单击】事件感兴趣，如果不感兴趣，说明只对它的【双击】事件感兴趣，但是【双击】事件不可能发生，那么流转回Init状态并把本次按键事件作为独立事件处理
                            // 如果感兴趣，那么执行相应的处理并流转回Init状态，然后处理本次的其它键按下事件。
                            if self.judge_interest(previous_key_up_event.code(), RuleType::Click) {
                                println!("你单击了{:?}键", KeyEvent::from(previous_key_up_event));
                                if let Some(action) = self.get_action_for(&previous_key_up_event.code(), &RuleType::Click) {
                                    self.perform_shortcut_action(&action);
                                }
                            }
                            self.update_state(State::Init);
                            self.accept(event);
                        }else if key_action == KeyAction::Pressed && previous_key_up_event.code() == key_code {
                            if self.judge_interest(previous_key_up_event.code(), RuleType::DoubleClick) {
                                // 处于 KeyUpFirstTime 状态，250毫秒内相同的键被再次按下了，那么【双击】行为发生了，流转回Init状态。
                                println!("你双击了{:?}键", Key::from(previous_key_up_event.code()));
                                if let Some(action) = self.get_action_for(&key_code, &RuleType::DoubleClick) {
                                    self.perform_shortcut_action(&action);
                                }
                                self.update_state(State::Init);
                            }else {
                                // 对当前键的【双击】事件不感兴趣，那么意味着需要透传当前键的双击事件，然后流转回Init状态
                                self.send_key_event(KeyAction::Pressed, key_code);
                                self.send_key_event(KeyAction::Released, key_code);
                                self.send_key_event(KeyAction::Pressed, key_code);
                                self.send_key_event(KeyAction::Released, key_code);
                                self.update_state(State::Init);
                            }
                        }
                    }
                }
            }
            EventSummary::RelativeAxis(axis_event, axis_code, _value) => {
                if axis_code == RelativeAxisCode::REL_WHEEL || axis_code == RelativeAxisCode::REL_WHEEL_HI_RES {
                    // 鼠标滚轮事件
                    match self.state {
                        State::KeyDownFirstTime(previous_key_down_event) => {
                            if self.judge_interest(previous_key_down_event.code(), RuleType::ScrollWheelWithKeyPressed) {
                                println!("【键持续按下并滚动鼠标滚轮】事件发生");
                                if let Some(action) = self.get_action_for(&previous_key_down_event.code(), &RuleType::ScrollWheelWithKeyPressed) {
                                    self.perform_shortcut_action(&action);
                                }
                            }else {
                                self.send_mouse_event(axis_event);
                            }
                        }
                       _ => {
                            self.send_mouse_event(axis_event);
                        }
                    }
                }
            }

            _ => {
            }
        }

    }

    fn timeout(&mut self) {
        match self.state {
            State::Init => {
                panic!("State::Init状态不应该存在超时事件");
            }
            State::KeyDownFirstTime(_) => {
                panic!("State::KeyDownFirstTime状态不应该存在超时事件");
            }
            State::KeyUpFirstTime(previous_key_up_event) => {
                // 当前状态是 KeyUpFirstTime 状态，意味着至少对【单击】、【双击】二者之一感兴趣
                if self.judge_interest(previous_key_up_event.code(), RuleType::Click) {
                    // 处于 KeyUpFirstTime 状态，250毫秒内没有任何键按下，那么【单击】行为发生了，流转回Init状态。
                    println!("你单击了{:?}键", Key::from(previous_key_up_event.code()));
                    if let Some(action) = self.get_action_for(&previous_key_up_event.code(), &RuleType::Click) {
                        self.perform_shortcut_action(&action);
                    }
                    self.update_state(State::Init);
                }else {
                    // 只对双击感兴趣，但是超时了，透传并流转回Init状态
                    self.send_key_event(KeyAction::Pressed, previous_key_up_event.code());
                    self.send_key_event(KeyAction::Released, previous_key_up_event.code());
                    self.update_state(State::Init);
                }
            }
        }
    }
}

struct FilteredMouseDevice {
    event_stream: EventStream,
    virtual_device: Arc<Mutex<VirtualDevice>>
}

impl FilteredMouseDevice {
    fn new(event_stream: EventStream, virtual_device: Arc<Mutex<VirtualDevice>>) -> Self {
        Self {
            event_stream, virtual_device
        }
    }

    async fn next_event(&mut self) -> io::Result<InputEvent> {
        loop {
            let mouse_event = self.event_stream.next_event().await?;

            match mouse_event.destructure() {
                EventSummary::Key(_, _, _) => {
                    // 按键事件
                    return Ok(mouse_event);
                }
                EventSummary::RelativeAxis(axis_event, axis_code, _value) => {
                    if axis_code == RelativeAxisCode::REL_X || axis_code == RelativeAxisCode::REL_Y {
                        // 鼠标移动事件，直接透传
                        send_mouse_event(self.virtual_device.clone(), axis_event);
                    }else if axis_code == RelativeAxisCode::REL_WHEEL || axis_code == RelativeAxisCode::REL_WHEEL_HI_RES {
                        // 滚轮事件
                        return Ok(mouse_event);
                    }
                }
                _ => {
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let config = read_config().await;

    let mut keyboard_device = Device::open(config.devices.keyboard.as_str()).expect(&format!("Failed to open device [{}]", config.devices.keyboard));
    keyboard_device.grab().expect("Failed to grab keyboard");

    let mut mouse_device = Device::open(config.devices.mouse.as_str()).expect(&format!("Failed to open device [{}]", config.devices.mouse));
    mouse_device.grab().expect("Failed to grab mouse");

    let mut keyboard_event_stream = keyboard_device.into_event_stream().expect("Failed to initialize event stream");
    let mouse_event_stream = mouse_device.into_event_stream().expect("Failed to initialize event stream");

    let virtual_device = Arc::new(Mutex::new(VirtualDevice::default().expect("Failed to initialize virtual device")));
    let mut filtered_mouse_device = FilteredMouseDevice::new(mouse_event_stream, virtual_device.clone());
    let mut state_machine = StateMachine::new(config, virtual_device.clone());

    loop {
        let timeout_future = state_machine.state.timeout();
        let next_keyboard_event_future = keyboard_event_stream.next_event();
        let next_mouse_event_future = filtered_mouse_device.next_event();

        select! {
            _ = timeout_future => {
                state_machine.timeout();
            }
            keyboard_event = next_keyboard_event_future => {
                state_machine.accept(keyboard_event.expect("Failed to accept keyboard event"));
            }
            mouse_event = next_mouse_event_future => {
                state_machine.accept(mouse_event.expect("Failed to accept mouse event"));
            }
        }
    }
}

async fn read_config() -> Config {
    let exe_path = std::env::current_exe().unwrap();
    println!("exe_path: {}", exe_path.display());

    let config_file = exe_path.parent().unwrap().join("config.toml");
    let config_toml = tokio::fs::read_to_string(&config_file).await.expect("Failed to read config.json");
    println!("config.toml:\n{}", config_toml);

    let config: Config = toml::from_str(&config_toml).unwrap();

    println!("config: {:?}", config);

    config
}
