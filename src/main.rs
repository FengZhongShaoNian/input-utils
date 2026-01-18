use evdev::{
    Device, EventStream, EventSummary, InputEvent, KeyCode, KeyEvent, RelativeAxisCode,
    RelativeAxisEvent,
};
use mouse_keyboard_input::VirtualDevice;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::{fs, future, io};
use strum::EnumIter;
use threadpool::ThreadPool;
use tokio::select;

#[derive(Debug, Deserialize)]
struct Config {
    devices: Devices,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
struct Devices {
    keyboard: Vec<String>,
    mouse: Vec<String>,
}
#[derive(Debug, Deserialize, Clone)]
struct Rule {
    /// 某个键
    key: Key,
    /// 对这个键的什么事件感兴趣
    rule_type: RuleType,
    /// 当感兴趣的事件发生时需要执行的操作
    action: Action,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, EnumIter, Hash)]
enum Key {
    LeftCtrl,
    RightCtrl,
    LeftAlt,
    RightAlt,
    LeftShift,
    RightShift,
    LeftMeta,  // 左侧的Win键
    RightMeta, // 右侧的Win键
    BtnSide,   // 鼠标的两个侧键中靠后的那个键
    BtnExtra,  // 鼠标的两个侧键中靠前的那个键
    BtnRight,  // 鼠标右键
}

impl Into<KeyCode> for Key {
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
            Key::BtnRight => KeyCode::BTN_RIGHT,
        }
    }
}

impl From<KeyCode> for Key {
    fn from(value: KeyCode) -> Self {
        match value {
            KeyCode::KEY_LEFTCTRL => Key::LeftCtrl,
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
            _ => panic!("Unsupported key {:?} for input", value),
        }
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let key_str = String::deserialize(deserializer)?;
        key_str
            .trim()
            .parse::<KeyCode>()
            .map_err(serde::de::Error::custom)
            .map(Key::from)
    }
}

#[derive(Copy, Clone, Debug, Deserialize, Eq, PartialEq, Hash)]
enum RuleType {
    /// 对某个键的【单击】事件感兴趣
    Click,

    /// 对某个键的【双击】事件感兴趣
    DoubleClick,

    /// 对【按下某个键并滚动鼠标滚轮】事件感兴趣
    ScrollWheelWithKeyPressed,
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
        D: Deserializer<'de>,
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
        let key_code = s
            .trim()
            .parse::<KeyCode>()
            .map_err(|_| format!("Unable to parse key: {}", s))?;
        Ok(key_code)
    }
}

impl<'de> Deserialize<'de> for HoldingKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
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
    /// 仅ScrollWheelWithKeyPressed类型的规则可以使用这种类型的快捷键
    ShortcutWithKeyHolding(ScrollShortcut),
}

/// 鼠标滚轮滚动方向以及对应的快捷键
/// 仅ScrollWheelWithKeyPressed类型的规则可以使用这种类型的快捷键
#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScrollShortcut {
    /// 向上滚的时候的快捷键（例如"KEY_LEFTSHIFT+KEY_TAB"）
    up: ShortcutString,

    /// 向下滚的时候的快捷键（例如："KEY_TAB"）
    down: ShortcutString,

    /// 需要持续按住的键（例如："KEY_LEFTALT"）
    holding_key: HoldingKey,
}

#[derive(Copy, Clone, Debug)]
enum WheelDirection {
    Up,
    Down,
}

/// 表示某个键感兴趣的事件类型的集合
type Interests = HashSet<RuleType>;

/// 【按下某个键并滚动鼠标滚轮】事件是否已经发生
type ScrollWheelWithKeyPressedHappened = bool;

#[derive(Clone, Debug)]
enum State {
    /// 初始状态
    Init,

    /// 只有对某个键的【单击】、【双击】、【按下某个键并滚动鼠标滚轮】这3个事件的任一事件感兴趣，才会流转到这个状态。
    /// 表示当前处于【某个键首次被按下】的状态。
    /// 可能发生的行为有：键持续按下并滚动鼠标滚轮
    /// 1. 处于 KeyDownFirstTime 状态，忽略鼠标移动事件（直接透传鼠标移动事件），下一个事件是滚轮事件，那么【键持续按下并滚动鼠标滚轮】行为发生了，不流转状态。
    /// 2. 处于 KeyDownFirstTime 状态，如果下一个事件是其它键的按下事件，那么透传，同时状态流转回Init状态。
    /// 3. 处于 KeyDownFirstTime 状态，如果下一个事件是当前键松开的事件，那么转为KeyUpFirstTime状态。
    /// 4. 处于 KeyDownFirstTime 状态，对应的键是鼠标右键，如果超时，那么透传并流转回Init状态。
    KeyDownFirstTime(KeyEvent, Interests, ScrollWheelWithKeyPressedHappened),

    /// 只有对某个键的【单击】、【双击】这2个事件的任一事件感兴趣，才会流转到这个状态。
    /// 某个键被首次松开。可能发生的行为有：单击、双击
    /// 1.处于 KeyUpFirstTime 状态，若对当前键的单击事件不感兴趣，那么判断是否需要关注它的双击事件，若只需要关注双击事件，那么不流转状态，若都不需要关注，那么透传并流转回Init状态
    /// 2.处于 KeyUpFirstTime 状态，250毫秒内相同的键没有被再次按下（250毫秒内没有任何键按下、下一个事件上其它键的按下事件），那么【单击】行为发生了，流转回Init状态。
    /// 3.处于 KeyUpFirstTime 状态，250毫秒内相同的键被再次按下了，那么【双击】行为发生了，流转回Init状态。
    KeyUpFirstTime(KeyEvent, Interests),
}

impl State {
    pub(crate) async fn timeout(&self) {
        match self {
            State::Init => {
                let future = future::pending();
                let () = future.await;
            }
            State::KeyDownFirstTime(_ev, interests, scroll_wheel_with_key_pressed_happened) => {
                // 针对此状态设置超时时间是为了避免按键被长时间摁住时，一些需要通过长按按键+移动鼠标的操作无法进行。
                // 例如：假如针对鼠标右键设置了双击规则，如果按下的键是鼠标右键，如果没有针对按键按下的状态设置超时事件，
                // 而鼠标右键也一直不松开，也没有其它的按键事件/滚轮事件发生，那么鼠标右键按下的事件会被拦截，从而导致无法使用鼠标手势等需要摁住某个键并移动鼠标的功能无法正常使用。

                if !interests.contains(&RuleType::ScrollWheelWithKeyPressed) {
                    // 如果某个按键对【按下某个键并滚动鼠标滚轮】事件不感兴趣，那么设置一个短一点的超时时间
                    tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
                } else if *scroll_wheel_with_key_pressed_happened == true {
                    // 如果【按下某个键并滚动鼠标滚轮】事件已经发生，那么就不需要设置超时了，说明用户本次操作的目的就是执行【按下某个键并滚动鼠标滚轮】这个操作
                    let future = future::pending();
                    let () = future.await;
                } else {
                    // 对【按下某个键并滚动鼠标滚轮】事件感兴趣，但是这个事件尚未发生，那么稍微设置一个长一点的超时时间，因为这个事件的发生可能要慢一点（相较于【单击】/【双击】而言）
                    tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                }
            }
            State::KeyUpFirstTime(_ev, _interests) => {
                tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum KeyAction {
    Pressed,
    Holding,
    Released,
}

impl From<i32> for KeyAction {
    fn from(value: i32) -> Self {
        match value {
            0 => KeyAction::Released,
            1 => KeyAction::Pressed,
            2 => KeyAction::Holding,
            _ => panic!("Unknown key action for value: {}", value),
        }
    }
}

/// 虚拟键盘鼠标设备
struct VirtualKeyboardMouse {
    device: Arc<Mutex<VirtualDevice>>,
    thread_pool: ThreadPool,
}

impl VirtualKeyboardMouse {
    fn new(device: VirtualDevice) -> Self {
        Self {
            device: Arc::new(Mutex::new(device)),
            thread_pool: ThreadPool::new(1), // 单线程，以确保事件的顺序
        }
    }

    fn send_key_event(&self, key_action: KeyAction, key_code: KeyCode) {
        let device = self.device.clone();
        // 放在单独的线程中执行以避免阻塞调用者线程
        self.thread_pool.execute(move || {
            let device = &mut device.lock().unwrap();
            if key_action == KeyAction::Pressed {
                device.press(key_code.0).unwrap();
            } else if key_action == KeyAction::Released {
                device.release(key_code.0).unwrap();
            }
        });
    }

    fn send_mouse_event(&self, event: RelativeAxisEvent) {
        let device = self.device.clone();
        // 放在单独的线程中执行以避免阻塞调用者线程
        self.thread_pool.execute(move || {
            let axis_code = event.code();
            let mut device = device.lock().unwrap();
            if axis_code == RelativeAxisCode::REL_X {
                device.move_mouse_x(event.value()).unwrap();
            } else if axis_code == RelativeAxisCode::REL_Y {
                device.move_mouse_y(-event.value()).unwrap();
            } else if axis_code == RelativeAxisCode::REL_WHEEL {
                device.scroll_y(event.value()).unwrap();
            } else if axis_code == RelativeAxisCode::REL_WHEEL_HI_RES {
                device.scroll_y(event.value()).unwrap();
            }
        });
    }
}

struct StateMachine {
    state: State,
    config: Config,

    /// 感兴趣的KeyCode
    interest_keys: Vec<KeyCode>,

    /// 某个Key感兴趣的所有事件类型
    key_interests: HashMap<KeyCode, Interests>,

    /// 与某个Key对应的规则
    key_rules: HashMap<KeyCode, Vec<Rule>>,

    virtual_keyboard_mouse: Arc<VirtualKeyboardMouse>,

    shortcut_with_key_holding: Option<Action>,
}

impl StateMachine {
    fn new(config: Config, virtual_keyboard_mouse: Arc<VirtualKeyboardMouse>) -> Self {
        let rules = &config.rules;
        let interest_keys = rules.iter().map(|rule| rule.key.into()).collect();
        let mut key_interests: HashMap<KeyCode, Interests> = HashMap::new();
        let mut key_rules: HashMap<KeyCode, Vec<Rule>> = HashMap::new();
        for rule in rules {
            let interests = key_interests.get_mut(&rule.key.into());
            if let Some(interests) = interests {
                interests.insert(rule.rule_type.into());
            } else {
                let mut interests = HashSet::new();
                interests.insert(rule.rule_type.into());
                key_interests.insert(rule.key.into(), interests);
            }

            let rules_for_key = key_rules.get_mut(&rule.key.into());
            if let Some(rules_for_key) = rules_for_key {
                rules_for_key.push(rule.clone());
            } else {
                let rules_for_key = vec![rule.clone()];
                key_rules.insert(rule.key.into(), rules_for_key);
            }
        }
        Self {
            state: State::Init,
            config,
            interest_keys,
            key_interests,
            key_rules,
            virtual_keyboard_mouse,
            shortcut_with_key_holding: None,
        }
    }

    fn send_key_event(&mut self, key_action: KeyAction, key_code: KeyCode) {
        self.virtual_keyboard_mouse
            .send_key_event(key_action, key_code);
    }

    fn send_mouse_event(&mut self, event: RelativeAxisEvent) {
        self.virtual_keyboard_mouse.send_mouse_event(event);
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

    fn perform_shortcut_action(
        &mut self,
        action: &Action,
        wheel_direction: Option<WheelDirection>,
    ) {
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
            Action::ShortcutWithKeyHolding(ScrollShortcut {
                up,
                down,
                holding_key,
            }) => {
                let wheel_direction = wheel_direction.unwrap_or(WheelDirection::Down);
                let ShortcutString(s) = match wheel_direction {
                    WheelDirection::Up => up,
                    WheelDirection::Down => down,
                };
                let HoldingKey(h) = holding_key;
                println!(
                    "Perform shortcut action: {}+{}, holding key is: {}",
                    h, s, h
                );
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

    fn release_holding_key(&mut self) {
        if let Some(Action::ShortcutWithKeyHolding(ScrollShortcut { holding_key, .. })) =
            &self.shortcut_with_key_holding
        {
            let HoldingKey(h) = holding_key;
            println!("Releasing holding key: {}", h);
            let holding_key = HoldingKey::get_key_codes(&h).unwrap();
            self.send_key_event(KeyAction::Released, holding_key);
            self.shortcut_with_key_holding = None;
        }
    }

    /// 判断key_code是否对指定的rule_type感兴趣，感兴趣则返回true，否则返回false
    fn judge_interest(&self, key_code: KeyCode, rule_type: RuleType) -> bool {
        if self.key_interests.contains_key(&key_code) {
            if let Some(rule_types) = self.key_interests.get(&key_code) {
                rule_types.contains(&rule_type)
            } else {
                false
            }
        } else {
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
        println!("Accepting input event: {:?}", event);
        let current_state = self.state.clone();
        match event.destructure() {
            EventSummary::Key(key_event, key_code, value) => {
                let key_action: KeyAction = value.into();
                match current_state {
                    State::Init => {
                        if key_action == KeyAction::Pressed
                            && self.interest_keys.contains(&key_code)
                        {
                            let interests = self.key_interests.get(&key_code).unwrap();
                            self.update_state(State::KeyDownFirstTime(
                                key_event,
                                interests.clone(),
                                false,
                            ));
                        } else {
                            self.send_key_event(key_action, key_code);
                        }
                    }
                    State::KeyDownFirstTime(
                        previous_key_down_event,
                        interests,
                        scroll_wheel_with_key_pressed_happened,
                    ) => {
                        // 当前状态是 KeyDownFirstTime 状态，意味着至少对之前按下的这个键的【单击】、【双击】、【按下并滚动鼠标滚轮】这3个事件之一感兴趣

                        if key_action == KeyAction::Pressed
                            && previous_key_down_event.code() != key_code
                        {
                            // 处于 KeyDownFirstTime 状态，如果下一个事件是其它键的按下事件，那么透传，同时状态流转回Init状态
                            self.send_key_event(KeyAction::Pressed, previous_key_down_event.code());
                            self.send_key_event(KeyAction::Pressed, key_event.code());
                            self.update_state(State::Init);
                        } else if key_action == KeyAction::Released
                            && previous_key_down_event.code() == key_code
                        {
                            if scroll_wheel_with_key_pressed_happened == true {
                                // 进入这里说明 ScrollWheelWithKeyPressed 事件已经发生
                                self.release_holding_key();
                                self.update_state(State::Init);
                                return;
                            }
                            // 处于 KeyDownFirstTime 状态，进入这里说明 ScrollWheelWithKeyPressed 事件没有发生，
                            // 判断是否对当前键的单击或者双击事件感兴趣，
                            // 如果感兴趣，那么流转为 KeyUpFirstTime 状态
                            // 如果都不感兴趣，而当前又能走到 KeyDownFirstTime 状态，说明只对当前键的 ScrollWheelWithKeyPressed 事件感兴趣，那么流转回Init状态
                            if interests.contains(&RuleType::Click)
                                || interests.contains(&RuleType::DoubleClick)
                            {
                                self.update_state(State::KeyUpFirstTime(key_event, interests));
                            } else {
                                // 只对当前键的 ScrollWheelWithKeyPressed 事件感兴趣
                                self.update_state(State::Init);
                            }
                        }
                    }
                    State::KeyUpFirstTime(previous_key_up_event, interests) => {
                        // 当前处于 KeyDownFirstTime ，说明至少对当前键的【单击】或者【双击】事件感兴趣

                        // 处于 KeyUpFirstTime 状态，如果下一个事件上其它键的按下事件，这意味着【单击】事件发生了，而且【双击】事件不可能发生
                        if key_action == KeyAction::Pressed
                            && previous_key_up_event.code() != key_code
                        {
                            // 判断是否对当前键的【单击】事件感兴趣，如果不感兴趣，说明只对它的【双击】事件感兴趣，但是【双击】事件不可能发生，那么流转回Init状态并把本次按键事件作为独立事件处理
                            // 如果感兴趣，那么执行相应的处理并流转回Init状态，然后处理本次的其它键按下事件。
                            if interests.contains(&RuleType::Click) {
                                println!("你单击了{:?}键", KeyEvent::from(previous_key_up_event));
                                if let Some(action) = self
                                    .get_action_for(&previous_key_up_event.code(), &RuleType::Click)
                                {
                                    self.perform_shortcut_action(&action, None);
                                }
                            }
                            self.update_state(State::Init);
                            self.accept(event);
                        } else if key_action == KeyAction::Pressed
                            && previous_key_up_event.code() == key_code
                        {
                            if interests.contains(&RuleType::DoubleClick) {
                                // 处于 KeyUpFirstTime 状态，250毫秒内相同的键被再次按下了，那么【双击】行为发生了，流转回Init状态。
                                println!("你双击了{:?}键", Key::from(previous_key_up_event.code()));
                                if let Some(action) =
                                    self.get_action_for(&key_code, &RuleType::DoubleClick)
                                {
                                    self.perform_shortcut_action(&action, None);
                                }
                                self.update_state(State::Init);
                            } else {
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
            EventSummary::RelativeAxis(axis_event, axis_code, value) => {
                if axis_code == RelativeAxisCode::REL_WHEEL
                    || axis_code == RelativeAxisCode::REL_WHEEL_HI_RES
                {
                    // 鼠标滚轮事件
                    match current_state {
                        State::KeyDownFirstTime(
                            previous_key_down_event,
                            interests,
                            scroll_wheel_with_key_pressed_happened,
                        ) => {
                            if interests.contains(&RuleType::ScrollWheelWithKeyPressed) {
                                // 经过实践发现，滚动一次鼠标滚轮，会同时REL_WHEEL和REL_WHEEL_HI_RES事件
                                // 为了避免滚动一次滚轮触发两次【键持续按下并滚动鼠标滚轮】事件，这里仅对REL_WHEEL进行响应
                                if axis_code == RelativeAxisCode::REL_WHEEL {
                                    println!("【键持续按下并滚动鼠标滚轮】事件发生");
                                    if let Some(action) = self.get_action_for(
                                        &previous_key_down_event.code(),
                                        &RuleType::ScrollWheelWithKeyPressed,
                                    ) {
                                        let wheel_direction = if value > 0 {
                                            WheelDirection::Up
                                        } else {
                                            WheelDirection::Down
                                        };
                                        self.perform_shortcut_action(
                                            &action,
                                            Some(wheel_direction),
                                        );
                                    }
                                    if scroll_wheel_with_key_pressed_happened == false {
                                        // 更新scroll_wheel_with_key_pressed_happened为true，避免KeyDownFirstTime状态的超时事件
                                        self.update_state(State::KeyDownFirstTime(
                                            previous_key_down_event,
                                            interests,
                                            true,
                                        ));
                                    }
                                }
                            } else {
                                self.send_mouse_event(axis_event);
                            }
                        }
                        _ => {
                            self.send_mouse_event(axis_event);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn timeout(&mut self) {
        match self.state {
            State::Init => {
                panic!("State::Init状态不应该存在超时事件");
            }
            State::KeyDownFirstTime(previous_key_down_event, _, _) => {
                println!("KeyDownFirstTime的超时事件发生了");
                // 如果KeyDownFirstTime的超时事件发生了，说明在按下这个键后的指定时间内没有别的动作（按下其它按键/松开按键/转动鼠标滚轮）
                // 那么意味着【单击】、【双击】、【按下某个键并滚动鼠标滚轮】都不是本次操作的目的，因此透传并流转会Init状态
                self.send_key_event(KeyAction::Pressed, previous_key_down_event.code());
                self.update_state(State::Init);
            }
            State::KeyUpFirstTime(previous_key_up_event, _) => {
                println!("KeyUpFirstTime的超时事件发生了");
                // 当前状态是 KeyUpFirstTime 状态，意味着至少对【单击】、【双击】二者之一感兴趣
                if self.judge_interest(previous_key_up_event.code(), RuleType::Click) {
                    // 处于 KeyUpFirstTime 状态，指定时间内没有任何键按下，那么【单击】行为发生了，流转回Init状态。
                    println!("你单击了{:?}键", Key::from(previous_key_up_event.code()));
                    if let Some(action) =
                        self.get_action_for(&previous_key_up_event.code(), &RuleType::Click)
                    {
                        self.perform_shortcut_action(&action, None);
                    }
                    self.update_state(State::Init);
                } else {
                    // 只对双击感兴趣，但是超时了，透传并流转回Init状态
                    self.send_key_event(KeyAction::Pressed, previous_key_up_event.code());
                    self.send_key_event(KeyAction::Released, previous_key_up_event.code());
                    self.update_state(State::Init);
                }
            }
        }
    }
}

struct FilteredDevice {
    event_stream: EventStream,
    virtual_keyboard_mouse: Arc<VirtualKeyboardMouse>,
}

impl FilteredDevice {
    fn new(event_stream: EventStream, virtual_keyboard_mouse: Arc<VirtualKeyboardMouse>) -> Self {
        Self {
            event_stream,
            virtual_keyboard_mouse,
        }
    }

    async fn next_event(&mut self) -> io::Result<InputEvent> {
        loop {
            let input_event = self.event_stream.next_event().await?;

            match input_event.destructure() {
                EventSummary::Key(_, _, value) => {
                    // 按键事件
                    // 忽略长按事件，因为长按事件会在按键被长按时源源不断地产生，如果不忽略这些事件的话，
                    // 这会导致状态机的超时事件失效（因为select!的作用是允许同时等待多个计算操作，
                    // 然后当其中一个操作完成时就退出等待，因此如果长按事件先到来了，超时事件就不会得到处理）
                    // 举个例子：如果打算使用【按下Super键然后按下鼠标右键并拖动鼠标】来调整窗口大小（Gnome可以通过GNOME Tweaks工具配置这个功能），
                    // 如果不忽略掉长按Super键产生的事件，当鼠标右键配置了双击规则，先按下Super键，接着按下鼠标右键，由于鼠标右键的配置了双击规则，
                    // 它需要一个超时事件来判断双击行为是否发生了，或者是否需要透传鼠标右键按下的事件，然而由于【长按事件】的存在，超时事件得不到处理，
                    // 鼠标右键按下的事件就会被状态机一直拿在手里，导致Gnome没有收到鼠标右键按下的消息，从而不会调整窗口大小
                    let long_pressed = value == 2;
                    if long_pressed {
                        // 啥也不做
                    }else {
                        return Ok(input_event);
                    }
                }
                EventSummary::RelativeAxis(axis_event, axis_code, _value) => {
                    if axis_code == RelativeAxisCode::REL_X || axis_code == RelativeAxisCode::REL_Y
                    {
                        // 鼠标移动事件，直接透传
                        self.virtual_keyboard_mouse.send_mouse_event(axis_event);
                    } else if axis_code == RelativeAxisCode::REL_WHEEL
                        || axis_code == RelativeAxisCode::REL_WHEEL_HI_RES
                    {
                        // 滚轮事件
                        return Ok(input_event);
                    }
                }
                // 忽略其它事件
                _ => {}
            }
        }
    }
}

fn open_device_for_event_stream(device_path: &str) -> Option<EventStream> {
    let device = Device::open(device_path).ok();
    if let Some(mut device) = device {
        match device.grab() {
            Ok(_) => match device.into_event_stream() {
                Ok(event_stream) => Some(event_stream),
                Err(e) => {
                    println!(
                        "[WARN] Failed to open event stream for device [{}]: {}",
                        device_path, e
                    );
                    None
                }
            },
            Err(e) => {
                println!("[WARN] Failed to grabbing device [{}]: {}", device_path, e);
                None
            }
        }
    } else {
        println!("[WARN] Could not open device: {}", device_path);
        None
    }
}

fn get_available_device(devices: &Vec<String>) -> Option<String> {
    for device in devices {
        let path = Path::new(&device);
        if fs::exists(path).expect(&format!("Failed to check device existence: {device}")) {
            println!("Found device [{}]", device);
            return Some(device.to_string());
        }
    }
    None
}

struct NullableDevice {
    device: Option<FilteredDevice>
}

impl NullableDevice {
    fn new(device: Option<FilteredDevice>) -> Self {
        Self { device }
    }

    async fn next_event(&mut self) -> io::Result<InputEvent> {
        loop {
            if let Some(ref mut device) = self.device {
                let event = device.next_event().await;
                return event;
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let config = read_config().await;

    let keyboard_device = get_available_device(&config.devices.keyboard);
    let mouse_device = get_available_device(&config.devices.mouse);
    if keyboard_device.is_none() && mouse_device.is_none() {
        panic!("Both keyboard devices and mouse devices are not found");
    }
    let mut keyboard_event_stream = if let Some(device) = keyboard_device {
        open_device_for_event_stream(&device)
    } else {
        None
    };
    let mouse_event_stream = if let Some(device) = mouse_device {
        open_device_for_event_stream(&device)
    } else {
        None
    };

    if keyboard_event_stream.is_none() && mouse_event_stream.is_none() {
        panic!("Both keyboard event stream and mouse event stream not found");
    }

    let virtual_keyboard_mouse = Arc::new(VirtualKeyboardMouse::new(
        VirtualDevice::default().expect("Failed to initialize virtual device"),
    ));
    let mut filtered_keyboard_device = keyboard_event_stream.map(|event_stream| {
        FilteredDevice::new(event_stream, virtual_keyboard_mouse.clone())
    });
    let mut filtered_mouse_device = mouse_event_stream.map(|event_stream| {
        FilteredDevice::new(event_stream, virtual_keyboard_mouse.clone())
    });
    let mut state_machine = StateMachine::new(config, virtual_keyboard_mouse.clone());

    let mut filtered_keyboard_device = NullableDevice::new(filtered_keyboard_device);
    let mut filtered_mouse_device = NullableDevice::new(filtered_mouse_device);

    loop {
        let timeout_future = state_machine.state.timeout();
        let next_keyboard_event_future = filtered_keyboard_device.next_event();
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
    let config_toml = tokio::fs::read_to_string(&config_file)
        .await
        .expect("Failed to read config.json");
    println!("config.toml:\n{}", config_toml);

    let config: Config = toml::from_str(&config_toml).unwrap();

    println!("config: {:?}", config);

    config
}
