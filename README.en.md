# Input Utils

## Introduction
Input Utils is a tool designed to handle input device events (such as keyboard and mouse). It can evaluate and execute corresponding actions based on rules defined in a configuration file, such as triggering keyboard shortcuts or processing specific key combinations.

## Features
- Supports loading rules from a configuration file.
- Supports recognizing keyboard and mouse events.
- Supports executing shortcut actions or maintaining key states based on rule types.
- Provides a state machine mechanism to manage the event processing flow.

## Configuration File
The configuration file uses the `TOML` format and is named `config.toml`, located in the `src` directory. It allows definition of devices, rule types, shortcut actions, and more.

## Usage
1. Ensure the Rust development environment is installed.
2. Clone the project locally:
   ```bash
   git clone https://gitee.com/fengzhongshaonian/input-utils
   ```
3. Navigate to the project directory and run:
   ```bash
   cargo run
   ```

## Project Structure
- `build.rs`: Build script.
- `src/main.rs`: Main program logic, including the state machine and event handling.
- `src/config.toml`: Configuration file defining devices and rules.

## Dependencies
- `tokio`: For asynchronous runtime.
- `serde`: For serialization and deserialization of configuration.
- `evdev`: For handling input device events (keyboard and mouse).

## Contributing
Issues and Pull Requests are welcome! Please follow the project's code style and provide clear commit messages.

## License
This project is licensed under the MIT License. See the LICENSE file in the repository for details.